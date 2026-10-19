import assert from 'node:assert/strict';
import { ALL_LIR_FAMILIES, LIR_EVIDENCE, LIR_FAMILIES, WORKER_LIR_FAMILIES, resolveLirFamilies } from './lir-policy.js';

assert.deepEqual(resolveLirFamilies(null, WORKER_LIR_FAMILIES), WORKER_LIR_FAMILIES);
assert.deepEqual(resolveLirFamilies(undefined), ALL_LIR_FAMILIES);
assert.deepEqual(resolveLirFamilies({ numbers: false }, WORKER_LIR_FAMILIES), { ...WORKER_LIR_FAMILIES, numbers: false });
assert.deepEqual(resolveLirFamilies('integer,loops'), { integer: true, numbers: false, memory: false, loops: true });
assert.deepEqual(resolveLirFamilies('', ALL_LIR_FAMILIES), { integer: false, numbers: false, memory: false, loops: false });
assert.throws(() => resolveLirFamilies('integer,bogus'), /unknown LIR family bogus/);
assert.throws(() => resolveLirFamilies({ bogus: true }), /unknown LIR family bogus/);
assert.deepEqual(Object.keys(WORKER_LIR_FAMILIES).sort(), [...LIR_FAMILIES].sort());
// Flags follow the recorded evidence, and every family has evidence with positive finite timings.
for (const family of LIR_FAMILIES) {
  const { primary, native, lir, others } = LIR_EVIDENCE[family];
  assert.ok(primary && Number.isFinite(native) && Number.isFinite(lir) && native > 0 && lir > 0 && Array.isArray(others), family);
  assert.equal(WORKER_LIR_FAMILIES[family], lir < native, family);
}
const copy = resolveLirFamilies(null, WORKER_LIR_FAMILIES); copy.memory = !copy.memory;
assert.notEqual(WORKER_LIR_FAMILIES.memory, copy.memory, 'resolve must return a copy');
console.log('PASS LIR family policy: flags follow recorded evidence, overrides, validation');
