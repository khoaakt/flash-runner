import assert from 'node:assert/strict';
import { extractIntegerTrace, lowerCapturedIntegerLeaf } from './nanojit-leaf.js';

function record(address, opcode, words) {
  const b = new Uint8Array((words.length + 1) * 4);
  const d = new DataView(b.buffer);
  words.forEach((value, i) => d.setUint32(i * 4, value, true));
  b[b.length - 1] = opcode;
  return { address, opcode, bytes: [...b] };
}
const r = [];
const add = (opcode, words = []) => { const value = record(0x1000 + 32 * r.length, opcode, words); r.push(value); return value.address; };
add(0); add(4, [0x100]); add(4, [0x101]); add(4, [0x102]);
const env = add(4, [0]), argc = add(4, [1]), argv = add(4, [2]);
add(6, [3]); add(0x39, [0x30000]);
while (r.length < 23) add(12, [env]);
const a = add(0x1b, [4, argv]), b = add(0x1b, [8, argv]);
const sum = add(0x5e, [b, a]); add(7, [sum]);
const capture = { records: r, code: 0x6000, nativePrefix: new Array(1024).fill(0) };
const output = extractIntegerTrace(capture);
assert.deepEqual(output.offsets, [4, 8]); assert.equal(output.argc, 2);
assert.match(output.source, /= addi /);
let called = 0;
assert.equal(lowerCapturedIntegerLeaf({ installCapturedIntegerLeaf(entry, core, native, source, offsets, count) {
  called++; assert.equal(entry, 0x6000); assert.equal(core, 0x30000); assert.equal(native.length, 1024);
  assert.deepEqual(offsets, [4, 8]); assert.equal(count, 2); assert.equal(source, output.source); return 1;
} }, capture).count, 1);
assert.equal(called, 1);
const mutate = (fn) => { const bad = structuredClone(capture); fn(bad); assert.equal(extractIntegerTrace(bad), null); };
mutate((c) => { c.records[24].bytes[0] = 7; });
mutate((c) => { c.records[24].bytes[4] = 0; c.records[24].bytes[5] = 0; }); // nonargv base
mutate((c) => { c.records[25].bytes[4] = c.records[25].address & 255; c.records[25].bytes[5] = c.records[25].address >>> 8; }); // self reference
mutate((c) => { c.records[25].opcode = 0x2a; }); // call result, not an integer op
mutate((c) => { c.records[25].address = c.records[24].address; });
const refused = lowerCapturedIntegerLeaf({ installCapturedIntegerLeaf() { throw new Error('native proof rejected'); } }, capture);
assert.equal(refused.count, 0); assert.equal(refused.reason, 'native proof rejected');
console.log('PASS raw integer return-slice parsing, ABI offsets and malformed-reference/native rejection');
const branch = structuredClone(capture); branch.records.splice(25);
const push = (opcode, words = []) => { const value = record(0x1000 + branch.records.length * 32, opcode, words); branch.records.push(value); return value; };
const cmp = push(0x40, [b, a]);
const jump = push(0x31, [0, cmp.address]);
push(7, [a]); const label = push(0x33); push(7, [b]);
const jumpBytes = Uint8Array.from(jump.bytes); new DataView(jumpBytes.buffer).setUint32(0, label.address, true); jump.bytes = [...jumpBytes];
const selection = extractIntegerTrace(branch);
assert.ok(selection); assert.match(selection.source, /cmovi/); assert.match(selection.source, /lti/);
const cyclicBranch = structuredClone(branch); cyclicBranch.records[26].bytes[0] = 0;
assert.equal(extractIntegerTrace(cyclicBranch), null);
const nested = structuredClone(branch);
nested.records.splice(27, 0, record(0, 0x41, [a, b]), record(0, 0x30, [0, 0]), record(0, 7, [b]), record(0, 0x33, []));
nested.records.forEach((item, i) => { item.address = 0x1000 + i * 32; });
const fix = (index, words) => { const bytes = Uint8Array.from(nested.records[index].bytes); const view = new DataView(bytes.buffer); words.forEach((value, i) => view.setUint32(i * 4, value, true)); nested.records[index].bytes = [...bytes]; };
const address = (index) => nested.records[index].address;
fix(23, [4, address(6)]); fix(24, [8, address(6)]);
fix(25, [address(24), address(23)]); fix(27, [address(23), address(24)]);
fix(26, [address(32), address(25)]);
fix(28, [address(30), address(27)]);
fix(29, [address(24)]); fix(31, [address(23)]); fix(33, [address(24)]);
const nestedTrace = extractIntegerTrace(nested);
assert.ok(nestedTrace);
assert.match(nestedTrace.source, /cmovi[\s\S]*cmovi/);
assert.match(nestedTrace.source, /cmovi[\s\S]*cmovi/);
console.log('PASS bounded conditional return diamond and branch-target rejection');
