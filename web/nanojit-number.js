// Number return-slice candidates for the SHA-bound .363 capture profile. These
// opcode names are hypotheses until number.rs proves all native effects/result.
const NUMBER = new Map([[0x72, ['negd', 1]], [0xa2, ['i2d', 1]], [0xa8, ['d2i', 1]], [0x75, ['addd', 2]],
  [0x76, ['subd', 2]], [0x77, ['muld', 2]], [0x78, ['divd', 2]]]);
// 0x07 reti, 0x2a call and 0x3f eqi appear only in NanoJIT's int() idiom (d2i + range check + helper call);
// 0x25 std is a receiver Number-field store
const OPS = new Set([0, 4, 6, 7, 9, 12, 0x1b, 0x1d, 0x23, 0x25, 0x29, 0x2a, 0x31, 0x33, 0x39, 0x3b, 0x3f, 0x44, ...NUMBER.keys()]);
export function numberCaptureOpcode(opcode, records) { return records < 128 && OPS.has(opcode); }

export function extractNumberTrace(capture) {
  try {
    const records = capture.records;
    if (!Array.isArray(records) || records.length < 24 || records.length > 128 || records[0].opcode !== 0) return null;
    const word = (r, n = 0) => new DataView(Uint8Array.from(r.bytes).buffer).getUint32(n, true);
    const operand = (r, n) => word(r, r.bytes.length - 4 * (n + 1));
    const byAddress = new Map(records.map((r, index) => [r.address, { ...r, index }]));
    if (byAddress.size !== records.length) return null;
    const params = records.filter((r) => r.opcode === 4 && word(r) < 3);
    if (params.length !== 3 || params.some((r, i) => word(r) !== i)) return null;
    const argv = params[2].address, core = records[8];
    if (core.opcode !== 0x39 || core.bytes.length !== 8) return null;
    // argc is not read from the capture: records 13/14 are unreferenced immediates and
    // are not the argument count (three Number arguments capture as 2). It is derived
    // below from the argv offsets actually read; Rust re-derives and compares it.
    // retd (9): a double. reti (7): an int, which here is only NanoJIT's int() idiom around d2i.
    const doubleReturns = records.filter((r) => r.opcode === 9), intReturns = records.filter((r) => r.opcode === 7);
    if (doubleReturns.length + intReturns.length !== 1) return null;
    const returns = doubleReturns.length ? doubleReturns : intReturns, intResult = intReturns.length === 1;
    if (returns[0].bytes.length !== 8) return null;
    // The receiver object is argv[0]. A double field load is an `ldd [receiver + offset]`, a field store an
    // `std [receiver + offset]`. Field access is bound to the SSA as a parameter whose binding has the high bit set.
    const FIELD = 0x80000000;
    const receiver = records.find((r) => r.opcode === 0x1b && r.bytes.length === 12 && operand(r, 1) === argv && (word(r) & 0xffff) === 0)?.address;
    const fieldStores = records.filter((r) => r.opcode === 0x25).map((r) => {
      if (r.bytes.length !== 16 || receiver === undefined || operand(r, 2) !== receiver) throw new Error('store outside the receiver');
      return { index: byAddress.get(r.address).index, field: word(r) & 0xffff, value: operand(r, 1) };
    });
    if (records.some((r) => r.opcode === 0x23 && receiver !== undefined && r.bytes.length === 16 && operand(r, 2) === receiver)) return null; // int field store: memory family
    const offsets = [], types = [], values = new Map(), visiting = new Set(), lines = [];  // lines: [record index, text]
    const kind = new Map();   // SSA name -> 'f64' | 'i32'
    function emit(address, before) {
      const r = byAddress.get(address);
      if (!r || r.index >= before || visiting.has(address)) throw new Error('invalid Number SSA reference');
      if (values.has(address)) return values.get(address);
      visiting.add(address);
      const name = `n${r.index}`;
      let instruction;
      if (r.opcode === 0x1d && receiver !== undefined && operand(r, 1) === receiver) {
        // A receiver field. After a store to the same field in LIR order the load yields the stored value (LIR is
        // in program order); a store to an overlapping different offset is refused.
        const field = word(r) & 0xffff;
        if (r.bytes.length !== 12 || field > 4088 || field % 4) throw new Error('bad receiver field load');
        const earlier = fieldStores.filter((st) => st.index < r.index);
        if (earlier.some((st) => st.field !== field && Math.abs(st.field - field) < 8)) throw new Error('overlapping receiver fields');
        const prior = earlier.filter((st) => st.field === field).at(-1);
        if (prior) { visiting.delete(address); const forwarded = emit(prior.value, r.index); values.set(address, forwarded); return forwarded; }
        const binding = (FIELD | field) >>> 0;
        let parameter = offsets.indexOf(binding);
        if (parameter < 0) { parameter = offsets.length; offsets.push(binding); types.push('f64'); }
        instruction = `argd ${parameter}`;
        kind.set(name, 'f64');
      } else if (r.opcode === 0x1d || r.opcode === 0x1b) {
        // 0x1d: a double argument (8 bytes); 0x1b: an int32 argument (4 bytes). Both packed from offset 4.
        const offset = word(r) & 0xffff, type = r.opcode === 0x1d ? 'f64' : 'i32';
        if (r.bytes.length !== 12 || operand(r, 1) !== argv || offset < 4 || offset > 60) throw new Error('non-Number argv load');
        let parameter = offsets.indexOf(offset);
        if (parameter < 0) { parameter = offsets.length; offsets.push(offset); types.push(type); }
        else if (types[parameter] !== type) throw new Error('argv slot read with two types');
        instruction = `${type === 'f64' ? 'argd' : 'parami'} ${parameter}`;
        kind.set(name, type);
      } else if (r.opcode === 0x3b) {
        // immd: the double bits are the record's two payload words. Rust binds them to the native
        // constant-pool slot the code actually loads from and rechecks that slot on every call.
        if (r.bytes.length !== 12) throw new Error('malformed immd');
        instruction = `immd bits:${((BigInt(word(r, 4)) << 32n) | BigInt(word(r, 0))).toString(16).padStart(16, '0')}`;
        kind.set(name, 'f64');
      } else {
        const op = NUMBER.get(r.opcode);
        if (!op || r.bytes.length !== 4 * (op[1] + 1)) throw new Error('unsupported Number expression');
        const args = [];
        for (let n = 1; n <= op[1]; n++) args.push(emit(operand(r, n), r.index));
        // i2d consumes an int32, every other operation consumes doubles; all of them produce a double.
        const wanted = op[0] === 'i2d' ? 'i32' : 'f64';
        if (args.some((arg) => kind.get(arg) !== wanted)) throw new Error('Number operand type mismatch');
        kind.set(name, op[0] === 'd2i' ? 'i32' : 'f64');
        instruction = `${op[0]} ${args.join(' ')}`;
      }
      lines.push([r.index, `${name} = ${instruction}`]);
      visiting.delete(address); values.set(address, name);
      return name;
    }
    const ret = byAddress.get(returns[0].address);
    let returned = operand(ret, 1);
    if (intResult) {
      // The int() idiom: d2i -> store slot; if (result == 0x80000000) slot = helper(call); return load slot. The
      // fallback only runs for results the replacement refuses, so the value is the d2i. Every element of the
      // idiom must be present and in order; Rust proves the native bytes independently.
      const load = byAddress.get(returned);
      if (!load || load.opcode !== 0x1b || load.bytes.length !== 12) throw new Error('int return is not a slot load');
      const slot = byAddress.get(operand(load, 1));
      if (!slot || slot.opcode !== 6) throw new Error('int() result slot is not a stack slot');
      const stores = records.filter((r) => r.opcode === 0x23 && r.bytes.length === 16 && operand(r, 2) === slot.address);
      const d2i = stores.map((r) => byAddress.get(operand(r, 1))).find((v) => v?.opcode === 0xa8);
      const call = stores.map((r) => byAddress.get(operand(r, 1))).find((v) => v?.opcode === 0x2a);
      if (stores.length !== 2 || !d2i || !call) throw new Error('int() idiom stores differ');
      const compare = records.find((r) => r.opcode === 0x3f && r.bytes.length === 12 && [operand(r, 1), operand(r, 2)].includes(d2i.address)
        && [operand(r, 1), operand(r, 2)].some((a) => { const k = byAddress.get(a); return k?.opcode === 0x39 && word(k) === 0x80000000; }));
      const branch = compare && records.find((r) => r.opcode === 0x31 && r.bytes.length === 12 && operand(r, 1) === compare.address);
      const label = branch && byAddress.get(operand(branch, 2));
      if (!branch || label?.opcode !== 0x33) throw new Error('int() range check differs');
      const order = [d2i, stores[0], compare, branch, call, stores[1], label, load, ret].map((r) => byAddress.get(r.address)?.index ?? r.index);
      if (order.some((index, i) => i && index <= order[i - 1])) throw new Error('int() idiom out of order');
      returned = d2i.address;
    }
    const value = emit(returned, ret.index);
    if (kind.get(value) !== (intResult ? 'i32' : 'f64')) throw new Error('Number return type differs');
    // Receiver-field stores in LIR order: the value subtree must exist in the SSA (and may read argv slots the
    // returned value does not, so this precedes the argc derivation). The text carries them as
    // `st <field> <name>` lines (not Trace instructions); Rust strips and resolves them.
    const stores = fieldStores.sort((a, b) => a.index - b.index).map((st) => {
      const name = emit(st.value, st.index);
      if (kind.get(name) !== 'f64') throw new Error('stored value is not a double');
      return `st ${st.field} ${name}`;
    });
    if (!offsets.length) return null;
    // argc is the parameter layout the code read: packed from offset 4 (int32 = 4 bytes, double = 8, no
    // padding). A gap is a run of unread parameters of unknown size, assumed to be doubles; a wrong guess only
    // makes the runtime argc guard refuse. Rust re-derives it.
    const order = offsets.map((offset, i) => [offset, types[i] === 'i32' ? 4 : 8]).filter(([offset]) => offset < FIELD).sort((a, b) => a[0] - b[0]);
    let next = 4, argc = 0;
    for (const [offset, size] of order) {
      if (offset < next || (offset - next) % 8) return null;
      argc += (offset - next) / 8 + 1; next = offset + size;
    }
    if (argc > 8) return null;
    // Native SSE operations are emitted in LIR record order, not in operand-DFS order. Rust compares
    // the exact operation sequence, so present the SSA in record order (operands always precede users).
    lines.sort((x, y) => x[0] - y[0]);
    return { core: word(core), offsets, argc,
      source: `params ${types.join(' ')}\n${[...lines.map((line) => line[1]), ...stores].join('\n')}\n${intResult ? 'reti' : 'retd'} ${value}` };
  } catch { return null; }
}

export function lowerCapturedNumber(emu, capture) {
  const extracted = extractNumberTrace(capture);
  if (!extracted) return { count: 0, reason: 'unsupported-number-return' };
  try {
    const count = emu.installCapturedNumber(capture.code, extracted.core,
      Uint8Array.from(capture.nativePrefix), extracted.source, extracted.offsets, extracted.argc);
    return { count, reason: '', number: true, source: extracted.source };
  } catch (e) { return { count: 0, reason: String(e.message || e) }; }
}
