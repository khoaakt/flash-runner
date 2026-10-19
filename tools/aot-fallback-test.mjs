// Real V8 API/dispatcher test of serialized offline x87 fallback and SMC sync.
// Prerequisites: cargo build --release -p emu --bin emu-aot; sh tools/build-wasm.sh
import assert from 'node:assert/strict';
import { mkdtempSync, readFileSync, writeFileSync, rmSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';
import { Emulator, EV } from '../web/emu.js';

const root = fileURLToPath(new URL('../', import.meta.url));
const scratch = mkdtempSync(join(process.env.TMPDIR || tmpdir(), 'aot-fallback-'));
// 1 + 1 in x87; store 2 on the same page as code (outside guarded instructions),
// load it in EBX, add 40 and exit_group(42). No runtime x86->wasm lowering needed.
const code = Buffer.from('d9e8d9e8dec1db1d000101008b1d0001010083c328b8fc000000cd80', 'hex');
const elf = Buffer.alloc(0x1300);
elf.set([0x7f, 0x45, 0x4c, 0x46, 1, 1, 1]);
elf.writeUInt16LE(2, 16); elf.writeUInt16LE(3, 18); elf.writeUInt32LE(1, 20);
elf.writeUInt32LE(0x10000, 24); elf.writeUInt32LE(52, 28); elf.writeUInt32LE(0x1200, 32);
elf.writeUInt16LE(52, 40); elf.writeUInt16LE(32, 42); elf.writeUInt16LE(1, 44);
elf.writeUInt16LE(40, 46); elf.writeUInt16LE(4, 48); elf.writeUInt16LE(3, 50);
for (const [at, value] of [[52,1], [56,0x1000], [60,0x10000], [64,0x10000], [68,0x144], [72,0x144], [76,7], [80,0x1000]]) elf.writeUInt32LE(value, at);
code.copy(elf, 0x1000); elf.writeUInt32LE(0x10000, 0x1140);
const names = Buffer.from('\0.text\0.init_array\0.shstrtab\0'); names.copy(elf, 0x1160);
function section(index, fields) { fields.forEach((value, i) => elf.writeUInt32LE(value, 0x1200 + index * 40 + i * 4)); }
section(1, [1,1,6,0x10000,0x1000,code.length,0,0,1,0]);
section(2, [7,14,3,0x10140,0x1140,4,0,0,4,4]);
section(3, [19,3,0,0,0x1160,names.length,0,0,1,0]);
try {
  const input = join(scratch, 'fp.elf'), prefix = join(scratch, 'fp'); writeFileSync(input, elf);
  const compiler = spawnSync(join(root, 'target/release/emu-aot'), [input, prefix, '--plugin-complete'], { encoding: 'utf8', timeout: 120000 });
  assert.equal(compiler.status, 0, compiler.stderr || compiler.error?.message);
  const index = JSON.parse(readFileSync(prefix + '.index.json', 'utf8'));
  assert.equal(index.coverage.fallback_instructions, 4);
  assert.equal(index.coverage.budget_frontiers, 0);
  const bundles = index.artifacts.map((a) => readFileSync(join(scratch, a.file)));
  assert.ok(bundles.every((b) => b[7] === 4));
  for (const mode of ['native', 'offline']) {
    const emu = await Emulator.create({ wasm: readFileSync(join(root, 'web/emu.wasm')), onLog: () => {} });
    emu.vfsNew(); emu.addFile('/fp', elf); emu.boot(32); emu.setJit(100000); emu.spawn('/fp', ['/fp'], []);
    if (mode === 'offline') {
      assert.throws(() => emu.installPortableAot(bundles[0].subarray(0, bundles[0].length - 1), 0), /rejected/);
      for (const bundle of bundles) assert.ok(emu.installPortableAot(bundle, 0) > 0);
    }
    let event;
    for (let run = 0; run < 100; run++) { event = emu.run(10000, 1); if (event.type !== EV.YIELD) break; }
    assert.equal(event.type, EV.EXITED, event.msg); assert.equal(event.arg, 42, emu.jitReport());
    const report = emu.jitReport();
    assert.match(report, new RegExp(`serialized_fallback_calls=${mode === 'offline' ? 4 : 0}(?:\\n|$)`));
    if (mode === 'offline') {
      const exports = index.artifacts.reduce((n, artifact) => n + artifact.regions, 0);
      assert.match(report, new RegExp(`\\[jit\\] regions=${exports} `));
      assert.match(report, /promoted_to_jit=0(?:\n|$)/);
    }
    console.log(`PASS ${mode}: x87 result and same-code-page store preserved${mode === 'offline' ? '; four serialized helper calls' : ''}`);
  }
} finally { rmSync(scratch, { recursive: true, force: true }); }
