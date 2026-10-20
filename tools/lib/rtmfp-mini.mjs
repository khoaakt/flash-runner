// A just-enough RTMFP (RFC 7016) server for the UDP tests: it completes Flash's handshake and key exchange and answers its NetConnection `connect`, so
// `NetConnection.Connect.Success` happens over UDP through the helper. It is not a usable server: one session at a time, no NetStream, NetGroup,
// P2P, retransmission or timers; everything is derived from RFC 7016 and checked against what Flash 32.0.0.363 really sends (tools/rtmfp-mini-test.mjs).
//
//   hello (0x30, session 0)       -> responder hello (0x70: tag echo, cookie, certificate-shaped blob)
//   initial keying (0x38)         -> responder initial keying (0x78: our session id and nonce, which carries our Diffie-Hellman public value);
//                                    shared secret = Oakley group 2 (RFC 2409, 1024 bit); both AES-128 keys come from HMAC-SHA256 over the secret
//                                    and the two nonces, packets use AES-128-CBC with a zero IV and a 16-bit one's complement checksum
//   session packets               -> user data chunks (0x10) are acknowledged (0x51); the first message of a flow carrying `connect` (AMF0) gets a
//                                    `_result` with NetConnection.Connect.Success on a return flow; a close request (0x0c) gets a close acknowledgement (0x4c)
import { createCipheriv, createDecipheriv, createDiffieHellman, createHmac, getDiffieHellman, randomBytes } from 'node:crypto';

const HANDSHAKE_KEY = Buffer.from('Adobe Systems 02');
export const CHUNK = { HELLO: 0x30, KEYING: 0x38, RESPONDER_HELLO: 0x70, RESPONDER_KEYING: 0x78, DATA: 0x10, ACK: 0x51, CLOSE: 0x0c, CLOSE_ACK: 0x4c };
// packet flags: bit 7 set in session packets, 0x08 timestamp present, 0x04 timestamp echo present, low two bits the mode (3 = startup, 1 = initiator, 2 = responder)
const FLAG_TIME = 0x08, FLAG_ECHO = 0x04, MODE_STARTUP = 0x03, MODE_RESPONDER = 0x02;
const RESPONDER_NONCE_PREFIX = Buffer.from('031a0000021e008102' + '0d02', 'hex');   // then the 128-byte Diffie-Hellman public value
const FLOW_SIGNATURE = Buffer.from('0054430400', 'hex');                             // flash.net.NetConnection

const hmac = (key, data) => createHmac('sha256', key).update(data).digest();
const aes = (kind, key, data) => { const c = (kind === 'enc' ? createCipheriv : createDecipheriv)('aes-128-cbc', key, Buffer.alloc(16)); c.setAutoPadding(false); return Buffer.concat([c.update(data), c.final()]); };

/** 16-bit one's complement sum of big-endian words (a trailing odd byte is the high byte of a last word). */
export function checksum(bytes) {
  let sum = 0;
  for (let i = 0; i < bytes.length; i += 2) sum += (bytes[i] << 8) | (i + 1 < bytes.length ? bytes[i + 1] : 0);
  while (sum > 0xffff) sum = (sum & 0xffff) + (sum >>> 16);
  return ~sum & 0xffff;
}

export const vluRead = (b, cur) => { let v = 0, x; do { x = b[cur.i++]; v = v * 128 + (x & 0x7f); } while (x & 0x80); return v; };
export const vluWrite = (n) => { const out = [n % 128]; n = Math.floor(n / 128); while (n) { out.unshift((n % 128) | 0x80); n = Math.floor(n / 128); } return Buffer.from(out); };

/** Packet = 4 bytes of scrambled session id + AES-CBC of (checksum, flags, [timestamp], [echo], chunks, 0xff padding). -> { sid, ok, flags, timestamp, echo, chunks } */
export function decodePacket(pkt, key = HANDSHAKE_KEY) {
  if (pkt.length < 4 + 16) throw new Error('short packet');
  const sid = (pkt.readUInt32BE(0) ^ pkt.readUInt32BE(4) ^ pkt.readUInt32BE(8)) >>> 0;
  const plain = aes('dec', key, pkt.subarray(4, 4 + Math.floor((pkt.length - 4) / 16) * 16));
  const ok = plain.readUInt16BE(0) === checksum(plain.subarray(2));
  const flags = plain[2]; let o = 3, timestamp = null, echo = null;
  if (flags & FLAG_TIME) { timestamp = plain.readUInt16BE(o); o += 2; }
  if (flags & FLAG_ECHO) { echo = plain.readUInt16BE(o); o += 2; }
  const chunks = [];
  while (o + 3 <= plain.length) {
    const type = plain[o];
    if (type === 0xff || type === 0x00) { o++; continue; }          // padding
    const len = plain.readUInt16BE(o + 1);
    if (o + 3 + len > plain.length) break;
    chunks.push({ type, data: plain.subarray(o + 3, o + 3 + len) }); o += 3 + len;
  }
  return { sid, ok, flags, timestamp, echo, chunks };
}

/** Build a packet addressed to session `sid`: opts { key, flags, timestamp, echo }. */
export function encodePacket(chunks, { sid = 0, key = HANDSHAKE_KEY, flags = 0x0b, timestamp = 0, echo = null } = {}) {
  const head = [flags, (timestamp >> 8) & 255, timestamp & 255];
  if (flags & FLAG_ECHO) head.push(((echo ?? 0) >> 8) & 255, (echo ?? 0) & 255);
  const parts = [Buffer.from(head)];
  for (const c of chunks) { const h = Buffer.alloc(3); h[0] = c.type; h.writeUInt16BE(c.data.length, 1); parts.push(h, c.data); }
  let inner = Buffer.concat(parts);
  inner = Buffer.concat([inner, Buffer.alloc((16 - ((inner.length + 2) % 16)) % 16, 0xff)]);
  const plain = Buffer.concat([Buffer.alloc(2), inner]); plain.writeUInt16BE(checksum(inner), 0);
  const enc = aes('enc', key, plain);
  const sidField = Buffer.alloc(4); sidField.writeUInt32BE((sid ^ enc.readUInt32BE(0) ^ enc.readUInt32BE(4)) >>> 0, 0);
  return Buffer.concat([sidField, enc]);
}

/** In a hello chunk (0x30): variable-length epd length, the epd, then a 16-byte tag. */
export function parseHello(data) {
  const cur = { i: 0 }, len = vluRead(data, cur);
  const epd = data.subarray(cur.i, cur.i + len), tag = data.subarray(cur.i + len, cur.i + len + 16);
  return { epd, tag, url: epd.subarray(2).toString('latin1') };
}

/** Initiator initial keying (0x38): initiator session id, cookie echo, key block (the DH public value last), nonce (its "certificate"), 0x58. */
export function parseKeying(data) {
  const cur = { i: 4 };
  const farId = data.readUInt32BE(0), cookieLen = vluRead(data, cur); cur.i += cookieLen;
  const keyLen = vluRead(data, cur), keyBlock = data.subarray(cur.i, cur.i + keyLen); cur.i += keyLen;
  const nonceLen = vluRead(data, cur), nonce = data.subarray(cur.i, cur.i + nonceLen);
  return { farId, cookieLen, dhPublic: keyBlock.subarray(keyBlock.length - 128), initiatorNonce: nonce };
}

/** (initiator -> responder, responder -> initiator) AES keys. The first is what decrypts Flash's session packets, which is how the formula was found. */
export function sessionKeys(shared, initiatorNonce, responderNonce) {
  return {
    request: hmac(shared, hmac(responderNonce, initiatorNonce)).subarray(0, 16),
    response: hmac(shared, hmac(initiatorNonce, responderNonce)).subarray(0, 16),
  };
}

// ---- AMF0: only what `connect` and its `_result` use ------------------------------------------------------------------------------
const amfStr = (t) => { const b = Buffer.from(t, 'utf8'), h = Buffer.alloc(3); h[0] = 2; h.writeUInt16BE(b.length, 1); return Buffer.concat([h, b]); };
const amfNum = (n) => { const b = Buffer.alloc(9); b.writeDoubleBE(n, 1); return b; };
const amfObj = (o) => {
  const parts = [Buffer.from([3])];
  for (const [k, v] of Object.entries(o)) { const kb = Buffer.from(k, 'utf8'), kh = Buffer.alloc(2); kh.writeUInt16BE(kb.length); parts.push(kh, kb, typeof v === 'number' ? amfNum(v) : amfStr(v)); }
  parts.push(Buffer.from([0, 0, 9]));
  return Buffer.concat(parts);
};
export function readAmf0(buf, cur = { i: 0 }) {
  const t = buf[cur.i++];
  switch (t) {
    case 0: { const v = buf.readDoubleBE(cur.i); cur.i += 8; return v; }
    case 1: return buf[cur.i++] !== 0;
    case 2: { const n = buf.readUInt16BE(cur.i); cur.i += 2; const s = buf.toString('utf8', cur.i, cur.i + n); cur.i += n; return s; }
    case 3: case 8: {
      if (t === 8) cur.i += 4;
      const o = {};
      for (;;) { const n = buf.readUInt16BE(cur.i); cur.i += 2; if (n === 0 && buf[cur.i] === 9) { cur.i++; return o; } const k = buf.toString('utf8', cur.i, cur.i + n); cur.i += n; o[k] = readAmf0(buf, cur); }
    }
    case 5: case 6: return null;
    default: throw new Error('unsupported AMF0 type ' + t);
  }
}

export class MiniRtmfpServer {
  /** opts: { sessionId, privateKey (a Buffer; tests only), onEvent(name, detail) } */
  constructor({ sessionId = 0x01020304, privateKey = null, onEvent = () => {} } = {}) {
    this.sessionId = sessionId; this.privateKey = privateKey; this.onEvent = onEvent;
    this.keys = null; this.farId = 0; this.t0 = Date.now(); this.flows = new Map(); this.connected = false; this.nextFlow = 3; this.seen = [];
  }
  now() { return Math.floor((Date.now() - this.t0) / 4) & 0xffff; }

  /** One datagram in, the datagrams to send back out (Buffers). */
  handle(msg) {
    let sid;
    try { sid = (msg.readUInt32BE(0) ^ msg.readUInt32BE(4) ^ msg.readUInt32BE(8)) >>> 0; } catch { return []; }
    if (this.keys && sid === this.sessionId) return this.session(msg);
    let p; try { p = decodePacket(msg); } catch { return []; }
    if (!p.ok) return [];
    const out = [];
    for (const c of p.chunks) {
      this.seen.push(c.type.toString(16));
      if (c.type === CHUNK.HELLO) {
        const h = parseHello(c.data); this.onEvent('hello', { url: h.url });
        const cert = Buffer.concat([Buffer.from([0x01, 0x0a, 0x41, 0x0e]), randomBytes(64), Buffer.from([0x02, 0x15, 0x02, 0x02, 0x15, 0x05, 0x02, 0x15, 0x0e])]);
        const data = Buffer.concat([Buffer.from([h.tag.length]), h.tag, Buffer.from([64]), randomBytes(64), cert]);
        out.push(encodePacket([{ type: CHUNK.RESPONDER_HELLO, data }], { timestamp: p.timestamp ?? 0 }));
      } else if (c.type === CHUNK.KEYING) {
        const k = parseKeying(c.data);
        const group = getDiffieHellman('modp2'), dh = createDiffieHellman(group.getPrime(), group.getGenerator());
        if (this.privateKey) dh.setPrivateKey(this.privateKey);       // tests only: a fixed key makes a recorded exchange replayable
        dh.generateKeys();
        const shared = dh.computeSecret(k.dhPublic);
        const respNonce = Buffer.concat([RESPONDER_NONCE_PREFIX, dh.getPublicKey()]);
        this.keys = sessionKeys(shared, k.initiatorNonce, respNonce); this.farId = k.farId;
        this.onEvent('keying', { farId: k.farId });
        const sidBuf = Buffer.alloc(4); sidBuf.writeUInt32BE(this.sessionId);
        const data = Buffer.concat([sidBuf, vluWrite(respNonce.length), respNonce, Buffer.from([0x58])]);
        out.push(encodePacket([{ type: CHUNK.RESPONDER_KEYING, data }], { sid: k.farId, timestamp: p.timestamp ?? 0 }));
      }
    }
    return out;
  }

  session(msg) {
    let p; try { p = decodePacket(msg, this.keys.request); } catch { return []; }
    if (!p.ok) return [];
    const replies = [];
    for (const c of p.chunks) {
      this.seen.push(c.type.toString(16));
      if (c.type === CHUNK.DATA) replies.push(...this.userData(c.data));
      else if (c.type === CHUNK.CLOSE) { this.onEvent('close', {}); replies.push({ type: CHUNK.CLOSE_ACK, data: Buffer.alloc(0) }); }
    }
    if (!replies.length) return [];
    return [encodePacket(replies, { sid: this.farId, key: this.keys.response, flags: 0x80 | FLAG_TIME | FLAG_ECHO | MODE_RESPONDER, timestamp: this.now(), echo: p.timestamp ?? 0 })];
  }

  /** User data chunk: flags, flow id, stage, delta, [options when flags & 0x80], message. Acknowledge it; answer `connect`. */
  userData(d) {
    const cur = { i: 1 }, flags = d[0], flow = vluRead(d, cur), stage = vluRead(d, cur); vluRead(d, cur);
    let signature = null;
    if (flags & 0x80) {
      const l = vluRead(d, cur); signature = d.subarray(cur.i, cur.i + l); cur.i += l;
      while (d[cur.i] !== 0) { const l2 = vluRead(d, cur); cur.i += l2; }        // further options (return flow association)
      cur.i++;
    }
    const out = [{ type: CHUNK.ACK, data: Buffer.concat([vluWrite(flow), vluWrite(0x7f), vluWrite(stage)]) }];
    if (signature && signature.equals(FLOW_SIGNATURE) && !this.flows.has(flow)) {
      this.flows.set(flow, { stage });
      const msg = d.subarray(cur.i);                                              // type (0x14 = AMF0 command), 4 bytes of time, then AMF0 values
      if (msg[0] === 0x14) {
        const c = { i: 5 };
        let name, txid, obj;
        try { name = readAmf0(msg, c); txid = readAmf0(msg, c); obj = readAmf0(msg, c); } catch { return out; }
        if (name === 'connect') {
          this.connected = true; this.onEvent('connect', { app: obj && obj.app, objectEncoding: obj && obj.objectEncoding });
          const body = Buffer.concat([Buffer.from([0x14, 0, 0, 0, 0]), amfStr('_result'), amfNum(txid), amfObj({ fmsVer: 'FMS/3,5,7,7009', capabilities: 31, mode: 1 }),
            amfObj({ level: 'status', code: 'NetConnection.Connect.Success', description: 'Connection succeeded', objectEncoding: (obj && obj.objectEncoding) ?? 0 })]);   // Flash wants its objectEncoding echoed
          const ret = this.nextFlow++;
          out.push({ type: CHUNK.DATA, data: Buffer.concat([Buffer.from([0x80]), vluWrite(ret), vluWrite(1), vluWrite(1), vluWrite(FLOW_SIGNATURE.length), FLOW_SIGNATURE, vluWrite(2), Buffer.from([0x0a]), vluWrite(flow), Buffer.from([0]), body]) });
        }
      }
    }
    return out;
  }
}
