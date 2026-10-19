// Unit test for the options page's zip import (deflate + stored, picks the plugin among other files).
//   node tools/zip-test.mjs path/to/runtime.zip [path/to/stored.zip]
import { readFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { extractRuntimeFromZip, isZip, inspectRuntime } from '../extension/options/runtime-store.js';

let failed = 0;
const check = (name, cond, extra = '') => { console.log((cond ? 'PASS ' : 'FAIL ') + name + (cond ? '' : '  ' + extra)); if (!cond) failed++; };
for (const f of process.argv.slice(2)) {
  const bytes = new Uint8Array(readFileSync(f));
  check(`${f}: recognised as a zip`, isZip(bytes));
  const r = await extractRuntimeFromZip(bytes);
  check(`${f}: plugin extracted and valid`, inspectRuntime(r.bytes).ok, r.name);
  console.log('   ', r.name, r.bytes.length, createHash('sha256').update(r.bytes).digest('hex').slice(0, 16));
}
let err = ''; try { await extractRuntimeFromZip(new Uint8Array([0x50, 0x4b, 3, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0])); } catch (e) { err = e.message; }
check('garbage zip is rejected with a message', /zip/.test(err), err);
process.exit(failed ? 1 : 0);
