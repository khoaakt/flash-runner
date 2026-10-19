import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { extractNumberTrace, lowerCapturedNumber, numberCaptureOpcode } from './nanojit-number.js';

function record(address, opcode, words = []) {
  const bytes = new Uint8Array(4 * (words.length + 1)), view = new DataView(bytes.buffer);
  words.forEach((word, i) => view.setUint32(i * 4, word, true)); bytes[bytes.length - 1] = opcode;
  return { address, opcode, bytes: [...bytes] };
}
function fixture(opcode) {
  const records = [], add = (op, words) => { const r = record(0x1000 + records.length * 32, op, words); records.push(r); return r.address; };
  add(0); add(4, [0x100]); add(4, [0x101]); add(4, [0x102]);
  add(4, [0]); add(4, [1]); const argv = add(4, [2]);
  add(6, [3]); add(0x39, [0x30000]);
  while (records.length < 14) add(12, [argv]);
  add(6, [opcode >= 0x75 ? 2 : 1]);
  while (records.length < 23) add(12, [argv]);
  const a = add(0x1d, [4, argv]); let result = a;
  if (opcode === 0x72) result = add(opcode, [a]);
  else if (opcode >= 0x75) { const b = add(0x1d, [12, argv]); result = add(opcode, [b, a]); }
  add(9, [result]);
  return { code: 0x6000, nativePrefix: [0xcc], records };
}
for (const opcode of [0x1d, 0x72, 0x75, 0x76, 0x77, 0x78]) {
  const capture = fixture(opcode), result = extractNumberTrace(capture);
  assert.ok(result); assert.deepEqual(result.offsets, opcode >= 0x75 ? [4, 12] : [4]);
  assert.equal(result.argc, opcode >= 0x75 ? 2 : 1);
  let called = 0;
  assert.equal(lowerCapturedNumber({ installCapturedNumber(entry, core, native, source, offsets, argc) {
    called++; assert.equal(entry, capture.code); assert.equal(core, 0x30000); assert.deepEqual([...native], [0xcc]);
    assert.equal(source, result.source); assert.deepEqual(offsets, result.offsets); assert.equal(argc, result.argc); return 1;
  } }, capture).count, 1); assert.equal(called, 1);
  assert.equal(lowerCapturedNumber({ installCapturedNumber() { throw Error('native proof refused'); } }, capture).reason, 'native proof refused');
  for (const mutate of [
    c => { c.records[23].bytes[0] = 8; },
    c => { c.records[23].bytes[4] = 0; },
    c => { c.records[23].opcode = 0x1b; },
    c => { c.records.at(-1).bytes.fill(0, 0, 4); },
    c => { c.records[24].address = c.records[23].address; },
    c => { c.records.at(-1).opcode = 7; },
  ]) { const bad = structuredClone(capture); mutate(bad); assert.equal(extractNumberTrace(bad), null); }
}
// argc comes from the argv offsets actually read, never from the unreferenced immediates at
// records 13/14 (real three-Number-argument captures carry 2 there).
for (const opcode of [0x1d, 0x75]) {
  const capture = fixture(opcode), baseline = extractNumberTrace(capture);
  for (const bogus of [0, 2, 7, 0xffffffff]) {
    const changed = structuredClone(capture);
    changed.records[14].bytes.splice(0, 4, ...new Uint8Array(new Uint32Array([bogus]).buffer));
    assert.deepEqual(extractNumberTrace(changed), baseline);
  }
}
{
  const capture = fixture(0x1d); const argv = capture.records[6].address;
  const records = capture.records.slice(0, 23); const add = (op, words) => { const r = record(0x1000 + records.length * 32, op, words); records.push(r); return r.address; };
  const a = add(0x1d, [4, argv]), b = add(0x1d, [12, argv]), c = add(0x1d, [20, argv]);
  const sum = add(0x75, [b, a]), result = add(0x77, [c, sum]); add(9, [result]);
  const three = extractNumberTrace({ ...capture, records });
  assert.deepEqual(three.offsets, [4, 12, 20]); assert.equal(three.argc, 3);
  assert.equal(three.source.split('\n').filter((line) => /= (addd|muld) /.test(line)).length, 2);
  assert.match(three.source, /muld n\d+ n\d+\nretd n\d+$/);
  // Unread leading/trailing parameters make the derived argc differ from the real call, which the
  // runtime argc guard refuses; the trace itself is still the exact read set.
  const last = records.slice(0, 23); const only = record(0x1000 + last.length * 32, 0x1d, [20, argv]); last.push(only);
  last.push(record(0x1000 + last.length * 32, 9, [only.address]));
  const third = extractNumberTrace({ ...capture, records: last });
  assert.deepEqual(third.offsets, [20]); assert.equal(third.argc, 3);
  for (const offset of [0, 8, 5, 64]) {
    const bad = structuredClone({ ...capture, records: last }); bad.records[23].bytes.splice(0, 2, offset & 255, offset >> 8);
    assert.equal(extractNumberTrace(bad), null, `offset ${offset}`);
  }
}
{
  // immd: bits come from the two payload words (low word first); constants are values, not parameters.
  const capture = fixture(0x1d); const argv = capture.records[6].address;
  const records = capture.records.slice(0, 23); const add = (op, words) => { const r = record(0x1000 + records.length * 32, op, words); records.push(r); return r.address; };
  const a = add(0x1d, [4, argv]), k = add(0x3b, [0, 0x40040000]), result = add(0x77, [k, a]); add(9, [result]);
  const scaled = extractNumberTrace({ ...capture, records });
  assert.deepEqual(scaled.offsets, [4]); assert.equal(scaled.argc, 1);
  assert.match(scaled.source, /= immd bits:4004000000000000\n/);
  assert.match(scaled.source, /= muld n\d+ n\d+\nretd/);
  const bad = structuredClone({ ...capture, records }); bad.records.find((r) => r.opcode === 0x3b).bytes.splice(0, 4);
  assert.equal(extractNumberTrace(bad), null, 'malformed immd');
}
{
  // int32 argv slots (0x1b) are 4 bytes, doubles (0x1d) 8, packed from offset 4; i2d (0xa2) converts exactly.
  const capture = fixture(0x1d); const argv = capture.records[6].address;
  const build = (make) => {
    const records = capture.records.slice(0, 23); const add = (op, words) => { const r = record(0x1000 + records.length * 32, op, words); records.push(r); return r.address; };
    const result = make(add, argv); add(9, [result]); return { ...capture, records };
  };
  const mixed = extractNumberTrace(build((add, argv) => { const i = add(0x1b, [4, argv]), d = add(0x1d, [8, argv]), c = add(0xa2, [i]); return add(0x75, [d, c]); }));
  assert.deepEqual(mixed.offsets, [4, 8]); assert.equal(mixed.argc, 2);
  assert.match(mixed.source, /^params i32 f64\nn\d+ = parami 0\nn\d+ = argd 1\nn\d+ = i2d n\d+\nn\d+ = addd n\d+ n\d+\nretd/);
  // lines are in LIR record order even when the return expression visits operands in another order
  const ascending = (source) => { const n = [...source.matchAll(/^n(\d+) =/gm)].map((m) => +m[1]); return JSON.stringify(n) === JSON.stringify([...n].sort((a, b) => a - b)); };
  assert.ok(ascending(mixed.source));
  const crossed = extractNumberTrace(build((add, argv) => {
    const a = add(0x1d, [4, argv]), b = add(0x1d, [12, argv]), x = add(0x75, [b, a]), y = add(0x76, [b, a]);
    return add(0x77, [x, y]);   // operand 1 is y (a later record) so the DFS reaches y before x
  }));
  assert.ok(crossed && ascending(crossed.source), crossed?.source);
  const reverse = extractNumberTrace(build((add, argv) => { const d = add(0x1d, [4, argv]), i = add(0x1b, [12, argv]), c = add(0xa2, [i]); return add(0x75, [c, d]); }));
  assert.deepEqual(reverse.offsets, [4, 12]); assert.equal(reverse.argc, 2); assert.match(reverse.source, /^params f64 i32\n/);
  assert.equal(extractNumberTrace(build((add, argv) => { const i = add(0x1b, [4, argv]); return add(0xa2, [i]); })).argc, 1);
  // a whole-double gap is unread parameters (argc counts them); half a double or an overlap is refused
  assert.equal(extractNumberTrace(build((add, argv) => { const i = add(0x1b, [20, argv]); return add(0xa2, [i]); })).argc, 3);
  assert.equal(extractNumberTrace(build((add, argv) => { const i = add(0x1b, [8, argv]); return add(0xa2, [i]); })), null, 'gap of 4 bytes');
  assert.equal(extractNumberTrace(build((add, argv) => { const i = add(0x1b, [4, argv]), d = add(0x1d, [4, argv]); return add(0x75, [add(0xa2, [i]), d]); })), null, 'slot read as int and double');
  // type errors never reach Rust: an int returned or used as a double, a double passed to i2d
  assert.equal(extractNumberTrace(build((add, argv) => add(0x1b, [4, argv]))), null, 'int return');
  assert.equal(extractNumberTrace(build((add, argv) => { const i = add(0x1b, [4, argv]), d = add(0x1d, [8, argv]); return add(0x75, [d, i]); })), null, 'int as double operand');
  assert.equal(extractNumberTrace(build((add, argv) => add(0xa2, [add(0x1d, [4, argv])]))), null, 'double into i2d');
  assert.equal(extractNumberTrace(build((add, argv) => { const base = capture.records[8].address; return add(0x1b, [4, base]); })), null, 'int load not from argv');
}
{
  // receiver Number fields: argv[0] is the receiver, ldd (0x1d) [receiver+off] reads a field, std (0x25) writes one
  const capture = fixture(0x1d); const argv = capture.records[6].address;
  const build = (make) => {
    const records = capture.records.slice(0, 23); const add = (op, words) => { const r = record(0x1000 + records.length * 32, op, words); records.push(r); return r.address; };
    const receiver = add(0x1b, [0, argv]);
    const result = make(add, argv, receiver); add(9, [result]); return { ...capture, records };
  };
  const F = (n) => (0x80000000 | n) >>> 0;
  const getter = extractNumberTrace(build((add, argv, r) => add(0x1d, [0x60, r])));
  assert.deepEqual(getter.offsets, [F(0x60)]); assert.equal(getter.argc, 0); assert.match(getter.source, /^params f64\nn\d+ = argd 0\nretd n\d+$/);
  const scaled = extractNumberTrace(build((add, argv, r) => { const k = add(0x1d, [4, argv]), x = add(0x1d, [0x60, r]); return add(0x77, [k, x]); }));
  assert.deepEqual([...scaled.offsets].sort((a, b) => a - b), [4, F(0x60)]); assert.equal(scaled.argc, 1);
  const setter = extractNumberTrace(build((add, argv, r) => { const v = add(0x1d, [4, argv]); add(0x25, [0x60, r, v]); return v; }));
  assert.deepEqual(setter.offsets, [4]); assert.equal(setter.argc, 1); assert.match(setter.source, /\nst 96 n\d+\nretd n\d+$/);
  // read-modify-write with the reload forwarded to the stored value; dy is read only by the second store
  const move = extractNumberTrace(build((add, argv, r) => {
    const dx = add(0x1d, [4, argv]), dy = add(0x1d, [12, argv]);
    const x = add(0x1d, [0x60, r]), nx = add(0x75, [dx, x]); add(0x25, [0x60, r, nx]);
    const y = add(0x1d, [0x58, r]), ny = add(0x75, [dy, y]); add(0x25, [0x58, r, ny]);
    return add(0x1d, [0x60, r]);
  }));
  assert.deepEqual([...move.offsets].sort((a, b) => a - b), [4, 12, F(0x58), F(0x60)]); assert.equal(move.argc, 2);
  assert.match(move.source, /\nst 96 (n\d+)\nst 88 n\d+\nretd \1$/);
  assert.equal(move.source.split('\n').filter((line) => line.startsWith('st ')).length, 2);
  // refusals: a store outside the receiver, an int store into the receiver, overlapping or unaligned fields
  const bad = (make) => extractNumberTrace(build(make));
  assert.equal(bad((add, argv, r) => { const v = add(0x1d, [4, argv]); add(0x25, [0x10, argv, v]); return v; }), null, 'store outside receiver');
  assert.equal(bad((add, argv, r) => { const v = add(0x1d, [4, argv]); add(0x23, [0x60, r, add(0x1b, [4, argv])]); return v; }), null, 'int field store');
  assert.equal(bad((add, argv, r) => { const v = add(0x1d, [4, argv]); add(0x25, [0x60, r, v]); return add(0x1d, [0x64, r]); }), null, 'overlapping field load after store');
  assert.equal(bad((add, argv, r) => add(0x1d, [0x62, r])), null, 'unaligned field');
  assert.equal(bad((add, argv, r) => add(0x1d, [0x1004, r])), null, 'field beyond the object window');
  assert.equal(bad((add, argv, r) => { const v = add(0x1d, [4, argv]); add(0x25, [0x60, r, add(0xa2, [add(0x1b, [4, argv])])]); return v; }), null, 'argv slot read as int and double');
}
assert.equal(numberCaptureOpcode(0x25, 100), true);
assert.equal(numberCaptureOpcode(0xa2, 100), true);
assert.equal(numberCaptureOpcode(0x3b, 100), true);
assert.equal(numberCaptureOpcode(0x1d, 100), true); assert.equal(numberCaptureOpcode(0x1d, 128), false);
assert.equal(numberCaptureOpcode(0x52, 30), false); // min diamond remains native
if (process.argv[2]) {
  const captures = JSON.parse(readFileSync(process.argv[2])).captures;
  const numbers = captures.map(extractNumberTrace).filter(Boolean);
  assert.ok(numbers.some(n => /retd n\d+$/.test(n.source) && !/= (negd|addd|subd|muld|divd)/.test(n.source)));
  for (const op of ['negd', 'addd', 'subd', 'muld', 'divd']) assert.ok(numbers.some(n => n.source.includes(`= ${op} `)), op);
  console.log(`PASS real captured Number identities/neg/add/sub/mul/div: ${numbers.length} candidates`);
}
console.log('PASS Number SSA offsets/argc, malformed graphs, native refusal and bounded capture');
