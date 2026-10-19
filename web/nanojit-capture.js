// Binary-verified observation profile. See docs/NANOJIT-ABI.md; not a generic LIns decoder.
import { lowerCapturedIntegerLeaf, pureCaptureOpcode } from './nanojit-leaf.js';
import { lowerCapturedNumber, numberCaptureOpcode } from './nanojit-number.js';
import { lowerCapturedLoop, loopCaptureOpcode } from './nanojit-loop.js';
import { lowerCapturedMemory } from './nanojit-memory.js';
import { ALL_LIR_FAMILIES, resolveLirFamilies } from './lir-policy.js';
const SHA256 = 'e357ccdc5f1bf81b89eaf10a6071a69bef5f758c9511a69586157cd3b72ea9f8';
const HOOKS = [
  [0x174002, 'e859de0600', 0x4e4a0001],
  [0x174016, '31c08b577885d2', 0x4e4a0002],
  [0x18a224, '5589e5565383ec10', 0x4e4a0003],
];
const unhex = (s) => Uint8Array.from(s.match(/../g).map((x) => parseInt(x, 16)));

export async function createNanojitCapture({ emu, plugin, log = () => {}, maxMethods = 32, maxRecords = 16384, lower = false, fullCapture = !lower, families = ALL_LIR_FAMILIES }) {
  const enabled = resolveLirFamilies(families, { integer: false, numbers: false, memory: false, loops: false });
  const hash = [...new Uint8Array(await crypto.subtle.digest('SHA-256', plugin))].map((b) => b.toString(16).padStart(2, '0')).join('');
  if (hash !== SHA256) throw new Error('NanoJIT capture is not verified for this runtime image');
  if (!Number.isInteger(maxMethods) || maxMethods < 1 || maxMethods > 256) throw new Error('invalid capture method limit');
  if (!Number.isInteger(maxRecords) || maxRecords < 1 || maxRecords > 65536) throw new Error('invalid capture record limit');
  const sizes = plugin.slice(0xcb41c0, 0xcb41c0 + 196);
  for (const [offset, hex] of HOOKS) {
    const expected = unhex(hex);
    if (!expected.every((v, i) => plugin[offset + i] === v)) throw new Error('NanoJIT hook profile mismatch');
  }
  let base = 0;
  const pending = new Map();
  const methods = new Map();
  const captures = [];
  let retainedBytes = 0;
  let loweredWork = 0;
  const stats = { before: 0, assembled: 0, installed: 0, rejected: 0, unsupported: 0, workLimited: 0, lowered: 0, loweredLoops: 0, loweredMemory: 0, loweredNumbers: 0, loweringRefusals: {} };
  const bytes = (address, size) => {
    const memory = emu.guest();
    if (!Number.isInteger(address) || address < 0 || !Number.isInteger(size) || size < 0 || address + size > memory.length) throw new Error('capture address outside guest RAM');
    emu.checkGuestReadable?.(address, size);
    return memory.subarray(address, address + size);
  };
  const word = (address) => { const b = bytes(address, 4); return new DataView(b.buffer, b.byteOffset, 4).getUint32(0, true); };
  const capture = (last) => {
    const records = [], seen = new Set();
    let cursor = last;
    for (let count = 0; count < maxRecords; count++) {
      if (cursor % 4 || seen.has(cursor)) throw new Error('unaligned/cyclic LIns stream');
      seen.add(cursor);
      const opcode = bytes(cursor + 3, 1)[0];
      if (!fullCapture && opcode !== 3 && !((enabled.integer || enabled.memory) && pureCaptureOpcode(opcode, records.length))
        && !(enabled.numbers && numberCaptureOpcode(opcode, records.length)) && !(enabled.loops && loopCaptureOpcode(opcode, records.length))) {
        const error = new Error('unsupported pure-leaf shape'); error.unsupportedLeaf = true; throw error;
      }
      const size = sizes[opcode];
      if (opcode === 0) { records.push({ address: cursor, opcode, bytes: [...bytes(cursor, 4)] }); return records.reverse(); }
      if (!size || size === 1 || size > 20 || size % 4) throw new Error(`unsupported LIns record size for opcode ${opcode}`);
      if (opcode === 3) { cursor = word(cursor - 4); continue; }
      records.push({ address: cursor, opcode, bytes: [...bytes(cursor + 4 - size, size)] });
      cursor -= size;
    }
    throw new Error('LIns capture exceeds record limit');
  };
  return {
    captures, stats,
    onPluginLoaded({ loadBias }) {
      base = loadBias;
      for (const [offset, hex, id] of HOOKS) emu.setEntryProbe(base + offset, id, unhex(hex));
      log(`[nanojit] verified capture hooks armed for ${hash.slice(0, 12)} at base=0x${base.toString(16)}`);
    },
    onEntryProbe({ tid, id }) {
      try {
        const sp = emu.reg(tid, 4);
        if (id === 0x4e4a0001) {
          stats.before++;
          if (lower && (stats.lowered >= 512 || stats.before > 100000 || loweredWork > 1000000)) { stats.workLimited++; return; }
          if ((captures.length >= maxMethods || retainedBytes >= 1024 * 1024) && !lower) return;
          const generator = emu.reg(tid, 6), assembler = word(sp), fragment = word(sp + 4), reader = word(sp + 8);
          const last = word(fragment + 4), method = word(generator + 0x28);
          if (word(generator + 8) !== fragment || word(reader) !== base + 0xe33218 || word(reader + 8) !== last) throw new Error('NanoJIT capture object invariants differ');
          const records = capture(last); loweredWork += records.length;
          const active = pending.get(tid) || [];
          if (active.length >= 16) throw new Error('nested compiler depth limit');
          active.push({ tid, method, fragment, assembler, last, records, observationSp: sp });
          pending.set(tid, active);
        } else if (id === 0x4e4a0002) {
          const stack = pending.get(tid);
          const index = stack?.findLastIndex((item) => item.observationSp === sp) ?? -1;
          if (index < 0) return; // an unsupported nested compilation was never captured
          const [item] = stack.splice(index, 1);
          if (!stack?.length) pending.delete(tid);
          if (word(item.assembler + 0x78) !== 0) throw new Error('native assembler failed');
          item.code = word(item.fragment + 0x14);
          item.prologue = word(item.fragment + 0x10);
          if (!item.code) throw new Error('native entry is null');
          if (methods.size >= maxMethods * 2) methods.delete(methods.keys().next().value);
          methods.set(item.method, item);
          stats.assembled++;
        } else if (id === 0x4e4a0003) {
          const method = word(sp + 8), code = word(sp + 12), item = methods.get(method);
          if (!item) return;
          methods.delete(method);
          if (item.code !== code) throw new Error('installed entry differs from captured fragment');
          // Native code pages may end shortly after a small method. Capture up
          // to the verified readable extent rather than rejecting a valid method
          // because an arbitrary 1 KiB suffix crosses an unmapped page.
          const firstLength = Math.min(1024, 4096 - (code & 4095));
          item.nativePrefix = [...bytes(code, firstLength)];
          if (firstLength < 1024) {
            try { item.nativePrefix.push(...bytes(code + firstLength, 1024 - firstLength)); }
            catch { /* leave a truncated capture; the verifier refuses incomplete native paths */ }
          }
          // Number-return call sites consume x87 ST0, never the raw i32 EAX
          // convention audited here. Return graph/native proof is still required;
          // no type inference is made from MethodInfo offsets beyond correlation.
          const retainedSize = item.nativePrefix.length + item.records.reduce((n, r) => n + r.bytes.length, 0);
          if (captures.length < maxMethods && retainedBytes + retainedSize <= 1024 * 1024) { captures.push(item); retainedBytes += retainedSize; }
          stats.installed++;
          if (lower) {
            let result = enabled.integer ? lowerCapturedIntegerLeaf(emu, item) : { count: 0, reason: 'family-disabled' };
            if (!result.count && enabled.numbers) {
              const number = lowerCapturedNumber(emu, item);
              if (number.count || number.reason !== 'unsupported-number-return') result = number;
            }
            if (!result.count && enabled.memory) {
              const memory = lowerCapturedMemory(emu, item);
              if (memory.count || memory.reason !== 'unsupported-receiver-memory') result = memory;
            }
            if (!result.count && enabled.loops) {
              const loopResult = lowerCapturedLoop(emu, item);
              if (loopResult.count) result = loopResult;
              else if ([63, 73].includes(item.records.length)) result = loopResult;
            }
            const n = result.count;
            // The work budget bounds records spent on methods that can actually be replaced. A method refused at
            // extraction/proof (most call-containing methods now pass the early opcode filter) is refunded, so the
            // refused majority cannot starve real candidates later in a long session; `before` still caps methods seen.
            if (!n) loweredWork = Math.max(0, loweredWork - item.records.length);
            if (!n) {
              const reason = result.reason.split(':', 1)[0];
              stats.loweringRefusals[reason] = (stats.loweringRefusals[reason] || 0) + 1;
            }
            item.lowering = result;
            stats.lowered += n;
            if (n && result.loop) stats.loweredLoops += n;
            if (n && result.memory) stats.loweredMemory += n;
            if (n && result.number) stats.loweredNumbers += n;
            if (n) log(`[nanojit] LIR->wasm installed method=0x${method.toString(16)} entry=0x${code.toString(16)}${result.loop ? ' loop=' + result.loop : ''}${result.memory ? ' memory=' + result.memory : ''}${result.number ? ' number' : ''}`);
          }
          if (!lower) log(`[nanojit] captured method=0x${method.toString(16)} entry=0x${code.toString(16)} records=${item.records.length}`);
        }
      } catch (e) {
        pending.delete(tid);
        if (e.unsupportedLeaf) { stats.unsupported++; return; }
        stats.rejected++;
        if (stats.rejected <= 8) log(`[nanojit] capture rejected: ${e.message || e}`);
      }
    },
  };
}
