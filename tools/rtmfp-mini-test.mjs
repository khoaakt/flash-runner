// Pins tools/lib/rtmfp-mini.mjs to a handshake packet Flash 32.0.0.363 really sent (captured from the UDP test): the checksum must match, the hello
// must parse, and what the module builds must decode back.
import { decodePacket, encodePacket, parseHello, responderHello, checksum, CHUNK } from './lib/rtmfp-mini.mjs';
import { createCipheriv } from 'node:crypto';

let failed = 0;
const check = (n, ok, extra = '') => { console.log((ok ? 'PASS ' : 'FAIL ') + n + (ok ? '' : '  ' + extra)); if (!ok) failed++; };

// the decrypted payload of one captured hello (checksum, flags, timestamp, chunk 0x30 of 46 bytes, padding), see docs/STATUS.md
const plain = Buffer.from('554e0b01a930002e1d1c0a72746d66703a2f2f3132372e302e302e313a31393335302f617070c8b763d3d12148323ff38cf748ab29e1ffffffffffffffffffff', 'hex');
check('the checksum of a captured Flash packet is reproduced', checksum(plain.subarray(2)) === plain.readUInt16BE(0), checksum(plain.subarray(2)).toString(16));
// re-encrypt it the way a peer would have, scramble the session id of 0 and decode it with the module
const c = createCipheriv('aes-128-cbc', Buffer.from('Adobe Systems 02'), Buffer.alloc(16)); c.setAutoPadding(false);
const enc = Buffer.concat([c.update(plain), c.final()]);
const head = Buffer.alloc(4); head.writeUInt32BE((enc.readUInt32BE(0) ^ enc.readUInt32BE(4)) >>> 0, 0);
const got = decodePacket(Buffer.concat([head, enc]));
check('a hello packet decodes: session 0, checksum ok, one 0x30 chunk', got.sid === 0 && got.ok && got.chunks.length === 1 && got.chunks[0].type === CHUNK.HELLO && got.timestamp === 0x01a9, JSON.stringify({ sid: got.sid, ok: got.ok, n: got.chunks.length }));
const hello = parseHello(got.chunks[0].data);
check('the hello carries the URL and a 16-byte tag', hello.url === 'rtmfp://127.0.0.1:19350/app' && hello.tag.length === 16 && hello.tag.toString('hex') === 'c8b763d3d12148323ff38cf748ab29e1', hello.url);
const reply = decodePacket(encodePacket([responderHello(hello.tag)], 5));
check('the responder hello round-trips and echoes the tag', reply.ok && reply.sid === 0 && reply.chunks[0].type === CHUNK.RESPONDER_HELLO && reply.chunks[0].data[0] === 16 && reply.chunks[0].data.subarray(1, 17).equals(hello.tag), JSON.stringify({ ok: reply.ok, type: reply.chunks[0]?.type }));
check('packets are a multiple of 16 bytes plus the 4-byte session id', (encodePacket([responderHello(hello.tag)]).length - 4) % 16 === 0);

console.log(failed ? `${failed} FAILED` : 'all ok');
process.exit(failed ? 1 : 0);
