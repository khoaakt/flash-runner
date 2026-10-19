// Return-slice frontend for the exact verified runtime profile. Rust independently
// proves equivalence against the complete native no-overflow method paths before
// emitting directly from these SSA operations and binding all native code bytes.
// The accepted return slice is integer SSA only. Its numeric opcode labels are
// cross-checked against a symbolic proof of the actual native method in wasm.
// We never infer heap accesses, native calls, or parameter types from opcode size.
export const INTEGER = new Map([
  [0x5d, ['negi', 1]], [0x5e, ['addi', 2]], [0x5f, ['subi', 2]], [0x60, ['muli', 2]],
  [0x63, ['noti', 1]], [0x64, ['andi', 2]], [0x65, ['ori', 2]], [0x66, ['xori', 2]],
  [0x67, ['lshi', 2]], [0x68, ['rshi', 2]], [0x69, ['rshui', 2]],
  [0x3f, ['eqi', 2]], [0x40, ['lti', 2]], [0x41, ['gti', 2]], [0x42, ['lei', 2]], [0x43, ['gei', 2]],
  [0x44, ['ltui', 2]], [0x45, ['gtui', 2]], [0x46, ['leui', 2]], [0x47, ['geui', 2]],
]);
const CAPTURE_OPS = new Set([0, 4, 6, 0x39, 0x1b, 0x23, 7, 12, 0x30, 0x31, 0x29, 0x33, ...INTEGER.keys()]);
export function pureCaptureOpcode(opcode, records) {
  return records < 256 && CAPTURE_OPS.has(opcode);
}
export function extractIntegerTrace(capture) {
  const records = capture.records;
  if (records.length < 24 || records.length > 256 || records[0].opcode !== 0) return null;
  const word = (r, offset = 0) => new DataView(Uint8Array.from(r.bytes).buffer).getUint32(offset, true);
  const operand = (r, n) => word(r, r.bytes.length - 4 * (n + 1));
  const byAddress = new Map(records.map((r, index) => [r.address, { ...r, index }]));
  if (byAddress.size !== records.length) return null;
  const params = records.filter((r) => r.opcode === 4 && word(r) < 3);
  if (params.length !== 3 || params.some((r, i) => word(r) !== i)) return null;
  const argv = params[2].address;
  const coreIns = records[8];
  if (coreIns.opcode !== 0x39) return null;
  const ret = records.filter((r) => r.opcode === 7);
  if (ret.length < 1 || ret.length > 8) return null;
  const values = new Map(), visiting = new Set(), lines = [], offsets = [];
  function emit(address, before) {
    const r = byAddress.get(address);
    if (!r || (typeof address === 'number' && r.index >= before) || visiting.has(address)) throw new Error('invalid captured SSA reference');
    if (values.has(address)) return values.get(address);
    visiting.add(address);
    const name = 'v' + values.size + '_' + r.index;
    let instruction;
    if (r.opcode === 0x39) {
      if (r.bytes.length !== 8) throw new Error('integer immediate record layout differs');
      instruction = `immi 0x${word(r).toString(16)}`;
    } else if (r.opcode === 0x1b) {
      const offset = word(r) & 0xffff;
      if (r.bytes.length !== 12 || operand(r, 1) !== argv || offset < 4 || offset > 64 || offset % 4) throw new Error('return slice contains nonargument loads');
      const index = offsets.indexOf(offset);
      const parameter = index < 0 ? offsets.push(offset) - 1 : index;
      instruction = `parami ${parameter}`;
    } else {
      const op = INTEGER.get(r.opcode);
      if (!op || r.bytes.length !== 4 * (op[1] + 1)) throw new Error('unsupported return expression');
      const args = [];
      for (let n = 1; n <= op[1]; n++) args.push(emit(operand(r, n), r.index));
      instruction = `${op[0]} ${args.join(' ')}`;
    }
    lines.push(`${name} = ${instruction}`);
    values.set(address, name); visiting.delete(address);
    return name;
  }
  const DATA = new Set([0x39, 0x1b, 0x23, 12, ...INTEGER.keys()]);
  function parseTail(index, depth) {
    if (depth > 4 || index >= records.length) throw new Error('conditional nesting exceeds the audited bound');
    while (DATA.has(records[index]?.opcode)) index++;
    const current = records[index];
    if (!current) throw new Error('conditional arm has no return');
    if (current.opcode === 7) return { value: emit(operand(current, 1), index), next: index + 1 };
    if (current.opcode !== 0x30 && current.opcode !== 0x31) throw new Error('unsupported captured control record');
    const target = byAddress.get(operand(current, 2));
    if (!target || target.opcode !== 0x33 || target.index <= index) throw new Error('invalid forward branch target');
    const condition = emit(operand(current, 1), index);
    const first = parseTail(index + 1, depth + 1);
    if (first.next !== target.index) throw new Error('conditional arm does not join its label');
    const second = parseTail(target.index + 1, depth + 1);
    const name = 'v' + lines.length + '_branch' + index;
    const yes = current.opcode === 0x31 ? first.value : second.value;
    const no = current.opcode === 0x31 ? second.value : first.value;
    lines.push(`${name} = cmovi ${condition} ${yes} ${no}`);
    return { value: name, next: second.next };
  }
  try {
    // Nested forward diamonds are parsed structurally. Native execution still
    // has to prove every arm and every byte; this parser only proposes the SSA.
    const tail = parseTail(20, 0);
    if (records.slice(tail.next).some((r) => r.opcode === 7 || r.opcode === 0x30 || r.opcode === 0x31)) return null;
    const value = tail.value;
    if (!offsets.length) return null;
    return { core: word(coreIns), offsets, argc: Math.max(...offsets) / 4,
      source: `params ${offsets.map(() => 'i32').join(' ')}\n${lines.join('\n')}\nreti ${value}` };
  } catch { return null; }
}
export function lowerCapturedIntegerLeaf(emu, capture) {
  const extracted = extractIntegerTrace(capture);
  if (!extracted) return { count: 0, reason: 'unsupported-return-slice' };
  try {
    const count = emu.installCapturedIntegerLeaf(capture.code, extracted.core,
      Uint8Array.from(capture.nativePrefix), extracted.source, extracted.offsets, extracted.argc);
    return { count, reason: '', source: extracted.source };
  } catch (e) { return { count: 0, reason: String(e.message || e) }; }
}
