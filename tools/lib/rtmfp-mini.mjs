// Just enough RTMFP (RFC 7016) to talk to Flash's first handshake step, so the UDP tests can show Flash accepts what comes back through the helper:
// decode a handshake packet (session 0, AES-128-CBC with the fixed key "Adobe Systems 02" and a zero IV), list its chunks, and answer a hello chunk (0x30)
// with a responder hello (0x70). This is not a server: there is no key exchange, so a session never starts; the proof is that Flash then sends its
// initiator initial keying chunk (0x38), which it only does after it has parsed the 0x70.
import { createCipheriv, createDecipheriv, randomBytes } from 'node:crypto';

const KEY = Buffer.from('Adobe Systems 02');
export const CHUNK = { HELLO: 0x30, KEYING: 0x38, RESPONDER_HELLO: 0x70, RESPONDER_KEYING: 0x78 };

/** 16-bit one's complement sum of big-endian words (a trailing odd byte is the high byte of a last word). */
export function checksum(bytes) {
  let sum = 0;
  for (let i = 0; i < bytes.length; i += 2) sum += (bytes[i] << 8) | (i + 1 < bytes.length ? bytes[i + 1] : 0);
  while (sum > 0xffff) sum = (sum & 0xffff) + (sum >>> 16);
  return ~sum & 0xffff;
}

/** -> { sid, ok, flags, timestamp, chunks: [{ type, data }] } ; `ok` says whether the checksum matched. */
export function decodePacket(pkt) {
  const words = [pkt.readUInt32BE(0), pkt.readUInt32BE(4), pkt.readUInt32BE(8)];
  const sid = (words[0] ^ words[1] ^ words[2]) >>> 0;
  const body = pkt.subarray(4, 4 + Math.floor((pkt.length - 4) / 16) * 16);
  const d = createDecipheriv('aes-128-cbc', KEY, Buffer.alloc(16)); d.setAutoPadding(false);
  const plain = Buffer.concat([d.update(body), d.final()]);
  const ok = plain.readUInt16BE(0) === checksum(plain.subarray(2));
  const flags = plain[2], hasTime = (flags & 0x08) !== 0;
  const timestamp = hasTime ? plain.readUInt16BE(3) : null;
  let o = hasTime ? 5 : 3; const chunks = [];
  while (o + 3 <= plain.length) {
    const type = plain[o];
    if (type === 0xff || type === 0x00) { o++; continue; }          // padding
    const len = plain.readUInt16BE(o + 1);
    if (o + 3 + len > plain.length) break;
    chunks.push({ type, data: plain.subarray(o + 3, o + 3 + len) }); o += 3 + len;
  }
  return { sid, ok, flags, timestamp, chunks };
}

/** Build a handshake-session packet from chunks [{type, data}] (session id 0). */
export function encodePacket(chunks, timestamp = 0, sid = 0) {
  const parts = [Buffer.from([0x0b, (timestamp >> 8) & 255, timestamp & 255])];
  for (const c of chunks) { const h = Buffer.alloc(3); h[0] = c.type; h.writeUInt16BE(c.data.length, 1); parts.push(h, c.data); }
  let inner = Buffer.concat(parts);
  const pad = (16 - ((inner.length + 2) % 16)) % 16;
  inner = Buffer.concat([inner, Buffer.alloc(pad, 0xff)]);
  const plain = Buffer.concat([Buffer.alloc(2), inner]); plain.writeUInt16BE(checksum(inner), 0);
  const c = createCipheriv('aes-128-cbc', KEY, Buffer.alloc(16)); c.setAutoPadding(false);
  const enc = Buffer.concat([c.update(plain), c.final()]);
  const head = Buffer.alloc(4); head.writeUInt32BE((sid ^ enc.readUInt32BE(0) ^ enc.readUInt32BE(4)) >>> 0, 0);
  return Buffer.concat([head, enc]);
}

/** In a hello chunk (0x30): variable-length epd length, the epd, then a 16-byte tag. */
export function parseHello(data) {
  let o = 0, len = 0, b;
  do { b = data[o++]; len = (len << 7) | (b & 0x7f); } while (b & 0x80);
  const epd = data.subarray(o, o + len), tag = data.subarray(o + len, o + len + 16);
  return { epd, tag, url: epd.subarray(2).toString('latin1') };
}

/** The responder hello (0x70): tag echo, a 64-byte cookie and a certificate-shaped blob (random: nothing here is ever verified). */
export function responderHello(tag) {
  const cert = Buffer.concat([Buffer.from([0x01, 0x0a, 0x41, 0x0e]), randomBytes(64), Buffer.from([0x02, 0x15, 0x02, 0x02, 0x15, 0x05, 0x02, 0x15, 0x0e])]);
  return { type: CHUNK.RESPONDER_HELLO, data: Buffer.concat([Buffer.from([tag.length]), tag, Buffer.from([64]), randomBytes(64), cert]) };
}
