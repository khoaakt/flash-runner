// Complete-stream frontend for the two exact Flash .363 counted-loop profiles.
// Rust checks every raw word and the complete native zero-poll path independently;
// the JS record count is only a cheap filter, never the equivalence proof.
const CAPTURE_OPS = new Set([0, 1, 2, 4, 6, 7, 12, 27, 35, 41, 47,
  48, 49, 51, 57, 64, 68, 94, 102, 103, 105, 192, 193, 194, 195]);
export function loopCaptureOpcode(opcode, records) { return records < 96 && CAPTURE_OPS.has(opcode); }

// Wire format: u32 record count, then (u32 address, u32 size, raw bytes)*.
// Do not serialize just the return slice: stores, guards, dead reads and side
// exits must remain available to the independent Rust structural verifier.
export function encodeLoopCapture(capture) {
  const records = capture?.records;
  if (!Array.isArray(records) || ![63, 73].includes(records.length)) return null;
  let size = 4;
  const seen = new Set();
  for (const record of records) {
    if (!Number.isInteger(record.address) || record.address < 4 || record.address > 0xfffffffc
      || record.address % 4 || seen.has(record.address) || !Array.isArray(record.bytes)
      || ![4, 8, 12, 16].includes(record.bytes.length)
      || record.bytes.some((v) => !Number.isInteger(v) || v < 0 || v > 255)
      || record.bytes.at(-1) !== record.opcode) return null;
    seen.add(record.address);
    size += 8 + record.bytes.length;
  }
  const metadata = new Uint8Array(size), view = new DataView(metadata.buffer);
  view.setUint32(0, records.length, true);
  let at = 4;
  for (const record of records) {
    view.setUint32(at, record.address, true);
    view.setUint32(at + 4, record.bytes.length, true);
    metadata.set(record.bytes, at + 8);
    at += 8 + record.bytes.length;
  }
  return metadata;
}

export function lowerCapturedLoop(emu, capture) {
  const metadata = encodeLoopCapture(capture);
  if (!metadata) return { count: 0, reason: 'unsupported-counted-loop-stream' };
  const coreRecord = capture.records[8];
  const core = new DataView(Uint8Array.from(coreRecord.bytes).buffer).getUint32(0, true);
  try {
    const count = emu.installCapturedLoop(capture.code, core,
      Uint8Array.from(capture.nativePrefix), metadata);
    return { count, reason: '', loop: capture.records.length === 63 ? 'sum' : 'xorshift' };
  } catch (e) { return { count: 0, reason: String(e.message || e) }; }
}
