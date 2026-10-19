// End-to-end API test: deployed LIR compiler and trusted offline artifacts in the real V8 dispatcher.
// Prerequisites: cargo build -p emu --bin emu-aot; sh tools/build-wasm.sh
import assert from 'node:assert/strict';
import { readFileSync, writeFileSync, mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';
import { Emulator, EV } from '../web/emu.js';

const root = fileURLToPath(new URL('../', import.meta.url));
const wasm = readFileSync(process.env.EMU_WASM || join(root, 'web/emu.wasm'));
const emu = await Emulator.create({ wasm,
  onLog: (s) => { if (process.env.ACCEL_TRACE) console.error(s); },
  onConsole: (fd, b) => { if (process.env.ACCEL_TRACE) process.stderr.write(b); },
});
const trace = emu.compileLir(`params i32 f64
i = parami 0
d = argd 1
r = d2i d
ok = eqi i r
xf ok 7
reti r`);
assert.deepEqual(trace(3, 3.9), [0, 3n]);
assert.deepEqual(trace(4, 3.9), [7, 0n]);
assert.deepEqual(trace(-2147483648, NaN), [0, -2147483648n]);
const negativeZero = emu.compileLir('params\nz = immd bits:8000000000000000\nretd z');
assert.equal(BigInt.asUintN(64, negativeZero()[1]), 1n << 63n);
assert.throws(() => emu.compileLir('params\nx = calli 123\nreti x'), /LIR compilation failed/);
console.log('PASS deployed LIR arithmetic, guards, SSE indefinite, signed zero and rejection');

// A tiny real ELF32 executable: sum 6..1 = 21, add 21, exit_group(42).
const code = Buffer.from('b90600000031db01cb4975fb83c315b8fc000000cd80', 'hex');
const elf = Buffer.alloc(0x1000 + code.length);
elf.set([0x7f, 0x45, 0x4c, 0x46, 1, 1, 1]);
elf.writeUInt16LE(2, 16); elf.writeUInt16LE(3, 18); elf.writeUInt32LE(1, 20);
elf.writeUInt32LE(0x10000, 24); elf.writeUInt32LE(52, 28);
elf.writeUInt16LE(52, 40); elf.writeUInt16LE(32, 42); elf.writeUInt16LE(1, 44);
for (const [offset, value] of [[52, 1], [56, 0x1000], [60, 0x10000], [64, 0x10000], [68, code.length], [72, code.length], [76, 5], [80, 0x1000]]) elf.writeUInt32LE(value, offset);
code.copy(elf, 0x1000);
emu.vfsNew(); emu.addFile('/work/aot-loop', elf); emu.boot(32); emu.setJit(100);
assert.equal(emu.x.emu_guest_readable(0x1000, 4), 0, 'unmapped host-readable backing memory is not a readable guest range');
assert.equal(emu.x.emu_guest_readable(0xfffffffc, 8), 0, 'overflowing guest range rejects');
assert.throws(() => emu.setEntryProbe(0xfffffffc, 7, new Uint8Array(8)), /entry probe rejected/);
emu.spawn('/work/aot-loop', ['/work/aot-loop'], []);
if (process.env.ACCEL_TRACE) emu.x.emu_set_trace(1);
const layout = emu.aotLayout();
assert.equal(layout.abi_version, 1);
const scratch = mkdtempSync(join(tmpdir(), 'flash-runner-accelerators-'));
try {
  const input = join(scratch, 'loop.elf');
  writeFileSync(input, elf);
  function compile(name, guestBase = layout.guest_base) {
    const prefix = join(scratch, name);
    const result = spawnSync(join(root, 'target/debug/emu-aot'), [input, prefix,
      '--entry', '0x10000', '--guest-base', String(guestBase),
      '--rtab', String(layout.rtab), '--wtab', String(layout.wtab)], { encoding: 'utf8' });
    assert.equal(result.status, 0, result.stderr || result.error?.message);
    return readFileSync(prefix + '.aot');
  }
  const bundle = compile('good');
  const wrongLayout = compile('wrong', layout.guest_base + 16);
  assert.throws(() => emu.installAot(wrongLayout), /AOT installation rejected/);
  assert.throws(() => emu.installAot(bundle.subarray(0, bundle.length - 1)), /AOT installation rejected/);
  const guest = emu.guest();
  guest[0x10000] ^= 1;
  assert.throws(() => emu.installAot(bundle), /AOT installation rejected/);
  guest[0x10000] ^= 1;
  const count = emu.installAot(bundle);
  assert.ok(count > 0);
  assert.throws(() => emu.installAot(bundle), /AOT installation rejected/);
  let result;
  for (let i = 0; i < 100; i++) {
    result = emu.run(10000, 1);
    if (result.type !== EV.YIELD) break;
  }
  assert.equal(result.type, EV.EXITED, result.msg);
  assert.equal(result.arg, 42, emu.jitReport());
  const report = emu.jitReport();
  assert.match(report, new RegExp(`offline regions installed=${count}`));
  assert.match(report, /calls=[1-9]\d*/);
  console.log(`PASS ${count} offline regions installed/executed; layout/byte/truncation/duplicate guards reject`);
} finally {
  rmSync(scratch, { recursive: true, force: true }); // only the directory this test just created
}
