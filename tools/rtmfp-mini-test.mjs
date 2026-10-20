// Pins tools/lib/rtmfp-mini.mjs to a real exchange: tools/lib/rtmfp-capture.json holds the datagrams Flash 32.0.0.363 sent to the library server (hello,
// initial keying, the session packet with NetConnection.connect, two more) and the server's fixed Diffie-Hellman private key, so the whole key exchange
// can be replayed offline. If the key formula or the packet format were wrong, Flash's session packet would not decrypt (checksum) and nothing here would pass.
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { MiniRtmfpServer, decodePacket, encodePacket, checksum, vluRead, vluWrite, readAmf0, CHUNK } from './lib/rtmfp-mini.mjs';

let failed = 0;
const check = (n, ok, extra = '') => { console.log((ok ? 'PASS ' : 'FAIL ') + n + (ok ? '' : '  ' + extra)); if (!ok) failed++; };

const cap = JSON.parse(readFileSync(join(dirname(fileURLToPath(import.meta.url)), 'lib/rtmfp-capture.json'), 'utf8'));
const [hello, keying, connect, ack, more] = cap.packets.map((h) => Buffer.from(h, 'hex'));
const events = [];
const srv = new MiniRtmfpServer({ privateKey: Buffer.from(cap.privateKey, 'hex'), onEvent: (n, d) => events.push([n, d]) });

// 1. the hello
const h = decodePacket(hello);
check('Flash\'s hello decodes: session 0, valid checksum, one 0x30 chunk', h.sid === 0 && h.ok && h.chunks.length === 1 && h.chunks[0].type === CHUNK.HELLO, JSON.stringify({ sid: h.sid, ok: h.ok }));
const r1 = srv.handle(hello);
const d1 = decodePacket(r1[0]);
check('the responder hello (0x70) round-trips and echoes the 16-byte tag', r1.length === 1 && d1.ok && d1.chunks[0].type === CHUNK.RESPONDER_HELLO && d1.chunks[0].data[0] === 16 && d1.chunks[0].data.subarray(1, 17).equals(h.chunks[0].data.subarray(-16)), JSON.stringify(events[0]));
check('the hello event carries the URL', events[0][0] === 'hello' && events[0][1].url === 'rtmfp://127.0.0.1:19350/app', JSON.stringify(events[0]));

// 2. the key exchange
const r2 = srv.handle(keying);
const d2 = decodePacket(r2[0]);
check('the initial keying is answered with a responder keying (0x78) addressed to the initiator\'s session id', r2.length === 1 && d2.ok && d2.sid === 0x02000000 && d2.chunks[0].type === CHUNK.RESPONDER_KEYING && d2.chunks[0].data.readUInt32BE(0) === srv.sessionId, JSON.stringify({ sid: d2.sid }));
check('the responder nonce is 139 bytes: an 11-byte prefix and a 128-byte Diffie-Hellman public value', d2.chunks[0].data.length === 4 + 2 + 139 + 1 && d2.chunks[0].data.subarray(4, 6).toString('hex') === '810b' && d2.chunks[0].data[d2.chunks[0].data.length - 1] === 0x58, d2.chunks[0].data.subarray(0, 8).toString('hex'));

// 3. Flash's session packet decrypts only if the derived key is right
const sp = decodePacket(connect, srv.keys.request);
check('Flash\'s session packet decrypts with the derived request key (checksum ok)', sp.ok && sp.chunks.length === 1 && sp.chunks[0].type === CHUNK.DATA, JSON.stringify({ ok: sp.ok, n: sp.chunks.length }));
check('and does not decrypt with the other key', !decodePacket(connect, srv.keys.response).ok);
const r3 = srv.handle(connect);
check('NetConnection.connect is recognised (app, objectEncoding 3) and answered with one packet', srv.connected && events.some(([n, d]) => n === 'connect' && d.app === 'app' && d.objectEncoding === 3) && r3.length === 1, JSON.stringify(events));
const reply = decodePacket(r3[0], srv.keys.response);
check('the reply is addressed to Flash\'s session, decrypts with the response key, and holds an ack (0x51) and a data chunk (0x10)', reply.ok && reply.sid === 0x02000000 && reply.chunks.map((c) => c.type).join() === `${CHUNK.ACK},${CHUNK.DATA}`, JSON.stringify({ ok: reply.ok, sid: reply.sid, chunks: reply.chunks.map((c) => c.type) }));
const ackData = reply.chunks[0].data, cur = { i: 0 };
check('the ack names Flash\'s flow 2 and stage 1', vluRead(ackData, cur) === 2 && vluRead(ackData, cur) === 0x7f && vluRead(ackData, cur) === 1);
const data = reply.chunks[1].data, c2 = { i: 1 };
{ const c2 = { i: 1 }; const f = [data[0], vluRead(data, c2), vluRead(data, c2), vluRead(data, c2), vluRead(data, c2)]; const sig = data.subarray(c2.i, c2.i + 5).toString('hex'); c2.i += 5; const optLen = vluRead(data, c2), optType = data[c2.i++], assoc = vluRead(data, c2);
  check('the return flow has options, its own id (3), stage 1, the NetConnection signature and is associated with the flow of Flash that asked', f.join() === '128,3,1,1,5' && sig === '0054430400' && optLen === 2 && optType === 0x0a && assoc === 2 && data[c2.i] === 0, JSON.stringify({ f, sig, optLen, optType, assoc })); }
const body = data.subarray(data.indexOf(0x14, 12));
const amf = { i: 5 }, name = readAmf0(body, amf), txid = readAmf0(body, amf);
readAmf0(body, amf); const info = readAmf0(body, amf);
check('the message is _result for transaction 1 with NetConnection.Connect.Success and Flash\'s objectEncoding', body[0] === 0x14 && name === '_result' && txid === 1 && info.code === 'NetConnection.Connect.Success' && info.level === 'status' && info.objectEncoding === 3, JSON.stringify({ name, txid, info }));

// 4. later packets from Flash are accepted (acked) and a retransmitted connect does not start a second flow
const before = srv.nextFlow;
check('a retransmission of the same connect is only acknowledged', srv.handle(connect).length === 1 && srv.nextFlow === before);
check('Flash\'s own ack and its second data message are accepted without error', Array.isArray(srv.handle(ack)) && Array.isArray(srv.handle(more)));

// 5. building blocks
const round = decodePacket(encodePacket([{ type: 0x51, data: Buffer.from([1, 2, 3]) }], { sid: 0x12345678, key: srv.keys.response, flags: 0x8e, timestamp: 7, echo: 9 }), srv.keys.response);
check('a session packet round-trips (session id, flags, timestamp, echo, chunk)', round.ok && round.sid === 0x12345678 && round.flags === 0x8e && round.timestamp === 7 && round.echo === 9 && round.chunks[0].data.equals(Buffer.from([1, 2, 3])));
check('checksum is the one\'s complement of the 16-bit sum', checksum(Buffer.from([0xff, 0xff])) === 0 && checksum(Buffer.from([0x00, 0x01, 0x00, 0x02])) === 0xfffc && checksum(Buffer.from([0x12])) === (~0x1200 & 0xffff));
check('variable-length integers round-trip (1, 127, 128, 16383, 16384, 2097152)', [1, 127, 128, 16383, 16384, 2097152].every((n) => vluRead(vluWrite(n), { i: 0 }) === n) && vluWrite(128).toString('hex') === '8100');
check('a close request is acknowledged with 0x4c', (() => { const c = encodePacket([{ type: CHUNK.CLOSE, data: Buffer.alloc(0) }], { sid: srv.sessionId, key: srv.keys.request, flags: 0x8d, timestamp: 1, echo: 1 }); const out = srv.handle(c); return out.length === 1 && decodePacket(out[0], srv.keys.response).chunks[0].type === CHUNK.CLOSE_ACK; })());
check('garbage and a wrong-checksum packet are ignored', srv.handle(Buffer.from('nope')).length === 0 && srv.handle(Buffer.alloc(40, 7)).length === 0);

console.log(failed ? `${failed} FAILED` : 'all ok');
process.exit(failed ? 1 : 0);
