// CRX3 packer: wraps a directory in a signed .crx without a browser (so a build machine needs only `zip` and the signing key).
// Layout (Chromium's crx3.proto): "Cr24", version 3, header length, CrxFileHeader, then the zip archive. The header carries the RSA public key
// and a SHA256-with-RSA signature over "CRX3 SignedData\0" + length(signed_header_data) + signed_header_data + archive; signed_header_data is
// {crx_id: first 16 bytes of SHA256(public key)}. The extension ID Chrome derives from the file is that crx_id spelled in a-p.
import { createHash, createPrivateKey, createPublicKey, createSign, createVerify } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const varint = (n) => { const out = []; while (n > 0x7f) { out.push((n & 0x7f) | 0x80); n = Math.floor(n / 128); } out.push(n); return Buffer.from(out); };
const field = (number, bytes) => Buffer.concat([varint(number * 8 + 2), varint(bytes.length), bytes]);   // wire type 2: length-delimited
const u32 = (n) => { const b = Buffer.alloc(4); b.writeUInt32LE(n); return b; };

export const extensionId = (publicKeyDer) => [...createHash('sha256').update(publicKeyDer).digest().subarray(0, 16)].map((b) => String.fromCharCode(97 + (b >> 4)) + String.fromCharCode(97 + (b & 15))).join('');

/** archive: a zip as a Buffer; privateKeyPem: the RSA key whose public half is the extension's `key`. */
export function wrapCrx3(archive, privateKeyPem) {
  const key = createPrivateKey(privateKeyPem);
  const publicKey = createPublicKey(key).export({ type: 'spki', format: 'der' });
  const signedData = field(1, createHash('sha256').update(publicKey).digest().subarray(0, 16));
  const signer = createSign('sha256');
  signer.update(Buffer.concat([Buffer.from('CRX3 SignedData\0', 'latin1'), u32(signedData.length), signedData, archive]));
  const proof = Buffer.concat([field(1, publicKey), field(2, signer.sign(key))]);
  const header = Buffer.concat([field(2, proof), field(10000, signedData)]);
  return Buffer.concat([Buffer.from('Cr24', 'latin1'), u32(3), u32(header.length), header, archive]);
}

/** Zip the directory's contents (manifest.json at the archive root) and wrap them. Returns { crx, id }. */
export function packDirectory(dir, privateKeyPem) {
  const work = mkdtempSync(join(tmpdir(), 'flash-crx-'));
  try {
    const zip = join(work, 'extension.zip');
    execFileSync('zip', ['-qr', '-X', zip, '.', '-x', '*.DS_Store'], { cwd: dir });
    const publicKey = createPublicKey(createPrivateKey(privateKeyPem)).export({ type: 'spki', format: 'der' });
    return { crx: wrapCrx3(readFileSync(zip), privateKeyPem), id: extensionId(publicKey) };
  } finally { rmSync(work, { recursive: true, force: true }); }
}

/** Reads a CRX3 back and checks its signature: { id, archive } or throws. For tests. */
export function readCrx3(buffer) {
  if (buffer.subarray(0, 4).toString('latin1') !== 'Cr24' || buffer.readUInt32LE(4) !== 3) throw new Error('not a CRX3 file');
  const headerLength = buffer.readUInt32LE(8), header = buffer.subarray(12, 12 + headerLength), archive = buffer.subarray(12 + headerLength);
  const readFields = (b) => { const out = []; for (let i = 0; i < b.length;) { let tag = 0, shift = 0, c; do { c = b[i++]; tag += (c & 0x7f) * 2 ** shift; shift += 7; } while (c & 0x80); let len = 0; shift = 0; do { c = b[i++]; len += (c & 0x7f) * 2 ** shift; shift += 7; } while (c & 0x80); out.push([tag >> 3, b.subarray(i, i + len)]); i += len; } return out; };
  const top = readFields(header), signedData = top.find(([n]) => n === 10000)?.[1], proofs = top.filter(([n]) => n === 2).map(([, b]) => readFields(b));
  if (!signedData || proofs.length !== 1) throw new Error('unexpected CRX3 header');
  const publicKey = proofs[0].find(([n]) => n === 1)[1], signature = proofs[0].find(([n]) => n === 2)[1];
  const verifier = createVerify('sha256');
  verifier.update(Buffer.concat([Buffer.from('CRX3 SignedData\0', 'latin1'), u32(signedData.length), signedData, archive]));
  if (!verifier.verify(createPublicKey({ key: publicKey, format: 'der', type: 'spki' }), signature)) throw new Error('CRX3 signature does not verify');
  const crxId = readFields(signedData).find(([n]) => n === 1)[1];
  if (!createHash('sha256').update(publicKey).digest().subarray(0, 16).equals(crxId)) throw new Error('crx_id is not the hash of the public key');
  return { id: extensionId(publicKey), archive };
}
