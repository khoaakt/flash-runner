// Real licensed Flash runtime, AS3-generated NanoJIT LIR, rendered self-check and live dispatch counters.
// Prerequisites: wasm/guest builds; node tools/build-plugin-aot.mjs; Haxe-built
// lir-leaf.swf, lir-arithmetic.swf and lir-loops.swf from tests/swf-src/.
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { join } from 'node:path';
import { createHash } from 'node:crypto';

const root = fileURLToPath(new URL('../', import.meta.url));
const plugin = 'vendor-private/libpepflashplayer-32.0.0.363.so';
const hash = createHash('sha256').update(readFileSync(join(root, plugin))).digest('hex');
const bundle = join(root, 'web/runtime-aot', hash.slice(0, 16) + '-functions.index.json');
for (const mode of ['native', 'lir', 'lir+aot']) {
  const args = ['web/flash-smoke.mjs', '--plugin', plugin, '--swf', 'tests/swf/lir-leaf.swf', '--seconds', '2', '--expect-pixel', '360,260,00ff00'];
  if (mode !== 'native') args.push('--lir');
  if (mode === 'lir+aot') args.push('--portable-index', bundle);
  const result = spawnSync(process.execPath, args, { cwd: root, encoding: 'utf8', timeout: 120000, maxBuffer: 8 * 1024 * 1024 });
  const output = result.stdout + result.stderr;
  assert.equal(result.status, 0, output || String(result.error));
  assert.match(output, /PASS expected pixel 360,260=00ff00/);
  assert.match(output, /crashed=no/);
  if (mode !== 'native') {
    assert.match(output, /LIR->wasm installed method=/);
    assert.match(output, /lir=[1-9]\d*/);
    assert.match(output, /completed_native_leaf_calls=[1-9]\d*/);
  }
  if (mode === 'lir+aot') assert.match(output, /aot=[1-9]\d*/);
  console.log(`PASS ${mode}: real Flash renders arithmetic/overflow self-check correctly`);
}
for (const mode of ['native', 'lir']) {
  const args = ['web/flash-smoke.mjs', '--plugin', plugin, '--swf', 'tests/swf/lir-arithmetic.swf', '--seconds', '10', '--wait-for-pass', '--read-timing-pixel', '--expect-pixel', '360,260,00ff00'];
  if (mode === 'lir') args.push('--lir');
  const result = spawnSync(process.execPath, args, { cwd: root, encoding: 'utf8', timeout: 120000, maxBuffer: 8 * 1024 * 1024 });
  const output = result.stdout + result.stderr;
  assert.equal(result.status, 0, output || String(result.error));
  if (mode === 'lir') {
    const lowered = Number(/"lowered":(\d+)/.exec(output)?.[1] || 0);
    assert.ok(lowered >= 20, `expected nested conditional coverage, got ${lowered}\n${output}`);
    const completed = Number(/completed_native_leaf_calls=(\d+)/.exec(output)?.[1] || 0);
    // Calls whose scheduling slice has insufficient budget intentionally execute
    // natively, so exact completed count varies slightly with scheduler/tier timing.
    assert.ok(completed >= 490000, `expected successful typed and dynamic calls, got ${completed}`);
  }
  console.log(`PASS arithmetic ${mode}: scalar/conditional methods and typed calls, actual result checked`);
}
for (const mode of ['native', 'lir']) {
  const args = ['web/flash-smoke.mjs', '--plugin', plugin, '--swf', 'tests/swf/lir-loops.swf', '--seconds', '10', '--wait-for-pass', '--expect-pixel', '360,260,00ff00'];
  if (mode === 'lir') args.push('--lir');
  const result = spawnSync(process.execPath, args, { cwd: root, encoding: 'utf8', timeout: 120000, maxBuffer: 8 * 1024 * 1024 });
  const output = result.stdout + result.stderr;
  assert.equal(result.status, 0, output || String(result.error));
  if (mode === 'lir') {
    assert.match(output, /LIR->wasm installed method=/);
    assert.match(output, /"loweredLoops":[1-9]/);
    assert.match(output, /completed_native_leaf_calls=[1-9]\d*/);
  }
  console.log(`PASS bounded loops ${mode}: actual AS3 sum/xorshift results checked`);
}
for (const mode of ['native', 'lir', 'lir+aot']) {
  const args=['web/flash-smoke.mjs','--plugin',plugin,'--swf','tests/swf/lir-memory.swf','--seconds','10','--wait-for-pass','--expect-pixel','360,260,00ff00'];
  if(mode!=='native')args.push('--lir');
  if(mode==='lir+aot')args.push('--portable-index',bundle);
  const result=spawnSync(process.execPath,args,{cwd:root,encoding:'utf8',timeout:120000,maxBuffer:8*1024*1024});
  const output=result.stdout+result.stderr;assert.equal(result.status,0,output||String(result.error));
  if(mode!=='native'){
    assert.match(output,/"loweredMemory":7/);assert.match(output,/memory=write/);
    const completed=Number(/completed_native_leaf_calls=(\d+)/.exec(output)?.[1]||0);
    assert.ok(completed>=139000,output);
  }
  console.log(`PASS receiver memory ${mode}: seven field methods, including ordered multi-field writes`);
}
for (const mode of ['native', 'lir']) {
  const args = ['web/flash-smoke.mjs', '--plugin', plugin, '--swf', 'tests/swf/lir-numbers.swf', '--seconds', '8', '--wait-for-pass', '--expect-pixel', '360,260,00ff00'];
  if (mode === 'lir') args.push('--lir');
  const result = spawnSync(process.execPath, args, { cwd: root, encoding: 'utf8', timeout: 120000, maxBuffer: 8 * 1024 * 1024 });
  const output = result.stdout + result.stderr;
  assert.equal(result.status, 0, output || String(result.error));
  if (mode === 'lir') {
    assert.match(output, /"loweredNumbers":6/);
    const completed = Number(/completed_native_leaf_calls=(\d+)/.exec(output)?.[1] || 0);
    assert.ok(completed >= 384, output);
  }
  console.log(`PASS numbers ${mode}: identity/neg/add/sub/mul/div returned through x87`);
}
for (const mode of ['native', 'lir', 'lir+aot']) {
  const args = ['web/flash-smoke.mjs', '--plugin', plugin, '--swf', 'tests/swf/lir-number-expr.swf', '--seconds', '10', '--wait-for-pass', '--expect-pixel', '360,260,00ff00'];
  if (mode !== 'native') args.push('--lir');
  if (mode === 'lir+aot') args.push('--portable-index', bundle);
  const result = spawnSync(process.execPath, args, { cwd: root, encoding: 'utf8', timeout: 120000, maxBuffer: 8 * 1024 * 1024 });
  const output = result.stdout + result.stderr;
  assert.equal(result.status, 0, output || String(result.error));
  assert.match(output, /crashed=no/);
  if (mode !== 'native') {
    // three-argument single operations, nested SSE programs (up to five steps, four inputs) and constants
    // (pool slots and code-immediate integers) int32 arguments (alone and mixed with doubles) and Std.int: 24 methods x 64 calls
    assert.match(output, /"loweredNumbers":24/, output);
    const completed = Number(/completed_native_leaf_calls=(\d+)/.exec(output)?.[1] || 0);
    assert.ok(completed >= 1500, output);   // the single 65536 * 65536 Std.int call is refused on purpose
  }
  console.log(`PASS number expressions ${mode}: 3-arg singles, nested add/sub/mul/div/neg programs and constants`);
}
for (const mode of ['native', 'lir', 'lir+aot']) {
  const args = ['web/flash-smoke.mjs', '--plugin', plugin, '--swf', 'tests/swf/lir-number-fields.swf', '--seconds', '10', '--wait-for-pass', '--expect-pixel', '360,260,00ff00'];
  if (mode !== 'native') args.push('--lir');
  if (mode === 'lir+aot') args.push('--portable-index', bundle);
  const result = spawnSync(process.execPath, args, { cwd: root, encoding: 'utf8', timeout: 120000, maxBuffer: 8 * 1024 * 1024 });
  const output = result.stdout + result.stderr;
  assert.equal(result.status, 0, output || String(result.error));
  assert.match(output, /crashed=no/);
  if (mode !== 'native') {
    // receiver Number fields: four getters/readers, a setter (FLD + FST) and a read-modify-write, 64 calls each.
    // The shadow-state self-check turns red if any field store or result differs.
    assert.match(output, /"loweredNumbers":7/, output);
    const completed = Number(/completed_native_leaf_calls=(\d+)/.exec(output)?.[1] || 0);
    assert.ok(completed >= 448, output);
  }
  console.log(`PASS number fields ${mode}: getters, field arithmetic, Std.int, setter and read-modify-write on a receiver`);
}
