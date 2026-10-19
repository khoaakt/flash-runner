// Word-sized receiver-field effects. Native symbolic verification is mandatory:
// a raw opcode/offset alone never authorizes a guest read or write.
import { INTEGER } from './nanojit-leaf.js';

export function extractMemoryTrace(capture) {
  try {
    const records = capture.records;
    if (records.length < 24 || records.length > 256 || records[0].opcode !== 0) return null;
    const word = (r, at = 0) => new DataView(Uint8Array.from(r.bytes).buffer).getUint32(at, true);
    const operand = (r, n) => word(r, r.bytes.length - 4 * (n + 1));
    const byAddress = new Map(records.map((r, i) => [r.address, { ...r, index: i }]));
    if (byAddress.size !== records.length || records[8].opcode !== 0x39) return null;
    const params = records.filter((r) => r.opcode === 4 && word(r) < 3);
    if (params.length !== 3 || params.some((r, i) => word(r) !== i)) return null;
    const argv = params[2].address;
    const receivers = records.filter((r) => r.opcode === 0x1b && r.bytes.length === 12 && operand(r, 1) === argv && (word(r) & 65535) === 0);
    if (receivers.length !== 1) return null;
    const receiver = receivers[0].address;
    const returns = records.filter((r) => r.opcode === 7);
    if (returns.length !== 1) return null;
    const body = records.slice(20, byAddress.get(returns[0].address).index);
    const stores = body.filter((r) => r.opcode === 0x23 && r.bytes.length === 16 && operand(r, 2) === receiver);
    if (stores.length > 8) return null;
    const fields = [...new Set(body.filter((r) => (r.opcode === 0x1b && operand(r, 1) === receiver)
      || (r.opcode === 0x23 && operand(r, 2) === receiver)).map((r) => word(r) & 65535))];
    if (!fields.length || fields.length > 16 || fields.some((n) => n > 4092 || n % 4)) return null;
    const argOffsets = body.filter((r) => r.opcode === 0x1b && operand(r, 1) === argv).map((r) => word(r) & 65535);
    if (argOffsets.some((n) => n > 64 || n % 4)) return null;
    const argc = Math.max(0, ...argOffsets) / 4;
    const bindings = [], values = new Map(), visiting = new Set(), lines = [], valueIds = new Map();
    function param(binding) {
      let index = bindings.indexOf(binding);
      if (index < 0) { index = bindings.length; bindings.push(binding); }
      return `parami ${index}`;
    }
    function emit(address, before) {
      const r = byAddress.get(address);
      if (!r || r.index >= before || visiting.has(address)) throw new Error('invalid memory SSA reference');
      if (values.has(address)) return values.get(address);
      visiting.add(address);
      const name = 'm' + r.index;
      let expression;
      if (r.opcode === 0x39 && r.bytes.length === 8) expression = `immi 0x${word(r).toString(16)}`;
      else if (r.opcode === 0x1b && r.bytes.length === 12) {
        const base = operand(r, 1), offset = word(r) & 65535;
        if (base === argv && offset <= argc * 4 && offset % 4 === 0) expression = param(offset);
        else if (base === receiver && fields.includes(offset)) {
          const previous = stores.findLast((store) => byAddress.get(store.address).index < r.index && (word(store) & 65535) === offset);
          if (previous) {
            const forwarded = emit(operand(previous, 1), byAddress.get(previous.address).index);
            values.set(address, forwarded); visiting.delete(address); return forwarded;
          }
          expression = param((0x80000000 | offset) >>> 0);
        } else throw new Error('unsupported memory base');
      } else {
        const operation = INTEGER.get(r.opcode);
        if (!operation || r.bytes.length !== 4 * (operation[1] + 1)) throw new Error('unsupported memory arithmetic');
        const args = [];
        for (let n = 1; n <= operation[1]; n++) args.push(emit(operand(r, n), r.index));
        expression = `${operation[0]} ${args.join(' ')}`;
      }
      values.set(address, name); visiting.delete(address); valueIds.set(name,lines.length); lines.push(`${name} = ${expression}`); return name;
    }
    const result = emit(operand(returns[0], 1), byAddress.get(returns[0].address).index);
    const effects=stores.map((store)=>{
      const value=emit(operand(store,1),byAddress.get(store.address).index);
      return [word(store)&65535,valueIds.get(value)];
    });
    return { core: word(records[8]), bindings, fields, store: stores.length ? word(stores[0]) & 65535 : null, argc,
      stores:effects,
      source: `params ${bindings.map(() => 'i32').join(' ')}\n${lines.join('\n')}\nreti ${result}` };
  } catch { return null; }
}

export function lowerCapturedMemory(emu, capture) {
  const program = extractMemoryTrace(capture);
  if (!program) return { count: 0, reason: 'unsupported-receiver-memory' };
  try {
    const count = emu.installCapturedMemoryEffects(capture.code, program.core, Uint8Array.from(capture.nativePrefix),
      program.source, program.bindings, program.fields, program.stores, program.argc);
    return { count, reason: '', memory: program.stores.length ? 'write' : 'read' };
  } catch (e) { return { count: 0, reason: String(e.message || e) }; }
}
