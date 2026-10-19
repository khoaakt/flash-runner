// Drives the socket helper the way Chrome does (native messaging framing on stdin/stdout) against an echo server, without a browser.
//   node native/helper-test.mjs [path/to/flash-helper]        (default: native/out/flash-helper-<this platform>)
import { spawn } from 'node:child_process';
import net from 'node:net';
import { mkdtempSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const arch = process.arch === 'arm64' ? 'arm64' : 'amd64';
const bin = process.argv[2] || join(root, 'native/out', process.platform === 'darwin' ? 'flash-helper-darwin' : `flash-helper-linux-${arch}`);
const ORIGIN = 'chrome-extension://njekchgflloenifdhaehploldjanaonk/';
let failed = 0;
const check = (n, ok, extra = '') => { console.log((ok ? 'PASS ' : 'FAIL ') + n + (ok ? '' : '  ' + extra)); if (!ok) failed++; };

const conns = new Set();
const echo = net.createServer((c) => { conns.add(c); c.on('close', () => conns.delete(c)); c.on('data', (d) => c.write(d)); });
await new Promise((r) => echo.listen(0, '127.0.0.1', r));
const echoPort = echo.address().port;

function start(cfg) {
  const dir = mkdtempSync(join(tmpdir(), 'flash-helper-test-')), cfgFile = join(dir, 'sockets.json');
  writeFileSync(cfgFile, JSON.stringify(cfg));
  const p = spawn(bin, [ORIGIN], { env: { ...process.env, FLASHRUNNER_SOCKETS_CONFIG: cfgFile }, stdio: ['pipe', 'pipe', 'inherit'] });
  const waiters = [], events = [];
  let buf = Buffer.alloc(0);
  p.stdout.on('data', (d) => {
    buf = Buffer.concat([buf, d]);
    while (buf.length >= 4) {
      const n = buf.readUInt32LE(0); if (buf.length < 4 + n) break;
      const m = JSON.parse(buf.subarray(4, 4 + n).toString('utf8')); buf = buf.subarray(4 + n);
      const i = waiters.findIndex((w) => w.pred(m)); if (i >= 0) { const [w] = waiters.splice(i, 1); w.res(m); } else events.push(m);
    }
  });
  const send = (m) => { const b = Buffer.from(JSON.stringify(m)); const h = Buffer.alloc(4); h.writeUInt32LE(b.length); p.stdin.write(Buffer.concat([h, b])); };
  const wait = (pred, ms = 4000) => {
    const i = events.findIndex(pred); if (i >= 0) return Promise.resolve(events.splice(i, 1)[0]);
    return new Promise((res, rej) => { const t = setTimeout(() => { const k = waiters.indexOf(w); if (k >= 0) waiters.splice(k, 1); rej(new Error('timeout waiting for a message')); }, ms); const w = { pred, res: (m) => { clearTimeout(t); res(m); } }; waiters.push(w); });
  };
  return { p, send, wait, events };
}

const h = start({ allowPrivate: true });
h.send({ t: 'hello', rid: 1 });
const hello = await h.wait((m) => m.rid === 1);
check('hello answers with version 2', hello.ok && hello.version === 2 && hello.allowPrivate === true, JSON.stringify(hello));
h.send({ t: 'connect', rid: 2, host: '127.0.0.1', port: echoPort });
const con = await h.wait((m) => m.rid === 2);
check('connect to the echo server', con.ok && con.id === 1 && con.remote.port === echoPort && con.local.ip === '127.0.0.1', JSON.stringify(con));
h.send({ t: 'write', rid: 3, id: con.id, data: Buffer.from('hello socket\n').toString('base64') });
const [wr, data] = await Promise.all([h.wait((m) => m.rid === 3), h.wait((m) => m.t === 'data' && m.id === con.id)]);
check('write acked and echoed back as data', wr.ok && Buffer.from(data.data, 'base64').toString() === 'hello socket\n', JSON.stringify([wr, data]));
// 1 MiB round trip, in order, possibly as several frames
const big = Buffer.alloc(1 << 20); for (let i = 0; i < big.length; i++) big[i] = i & 255;
h.send({ t: 'write', rid: 4, id: con.id, data: big.toString('base64') });
let got = 0, okBytes = true;
while (got < big.length) { const d = await h.wait((m) => m.t === 'data' && m.id === con.id, 8000); const b = Buffer.from(d.data, 'base64'); for (let i = 0; i < b.length; i++) if (b[i] !== ((got + i) & 255)) okBytes = false; got += b.length; }
check('1 MiB write round-trips intact', got === big.length && okBytes, `got ${got}`);
// pause stops delivery, resume restarts it
h.send({ t: 'pause', id: con.id });
h.send({ t: 'write', rid: 5, id: con.id, data: Buffer.from('after pause').toString('base64') });
await h.wait((m) => m.rid === 5);
const early = await h.wait((m) => m.t === 'data' && m.id === con.id, 600).then(() => true, () => false);
h.send({ t: 'resume', id: con.id });
const late = await h.wait((m) => m.t === 'data' && m.id === con.id, 3000).then((m) => Buffer.from(m.data, 'base64').toString(), () => null);
check('pause holds data back, resume releases it', !early && late === 'after pause', JSON.stringify({ early, late }));
// second socket + server going away -> close event
h.send({ t: 'connect', rid: 6, host: '127.0.0.1', port: echoPort });
const con2 = await h.wait((m) => m.rid === 6);
echo.close(); for (const c of conns) c.destroy();
const closed = await h.wait((m) => m.t === 'close' && m.id === con2.id, 4000).then(() => true, () => false);
check('echo server going away -> close event', closed);
// closing from our side, then writing, fails cleanly
h.send({ t: 'close', id: con.id });
h.send({ t: 'write', rid: 7, id: con.id, data: 'AA==' });
const wc = await h.wait((m) => m.rid === 7);
check('write on a closed socket -> EPIPE', !wc.ok && wc.code === 'EPIPE', JSON.stringify(wc));
h.send({ t: 'connect', rid: 8, host: 'nope.invalid', port: 80 });
const nf = await h.wait((m) => m.rid === 8, 8000);
check('unknown host -> ENOTFOUND', !nf.ok && nf.code === 'ENOTFOUND', JSON.stringify(nf));
h.send({ t: 'connect', rid: 9, host: '127.0.0.1', port: 1 });
const ref = await h.wait((m) => m.rid === 9, 8000);
check('closed port -> ECONNREFUSED', !ref.ok && ref.code === 'ECONNREFUSED', JSON.stringify(ref));
h.p.stdin.end();
const code = await new Promise((r) => h.p.on('exit', r));
check('helper exits when Chrome closes the pipe', code === 0, `exit ${code}`);
// default configuration: private destinations are refused, host globs apply
const h2 = start({ allowHosts: ['*.example.com'] });
h2.send({ t: 'connect', rid: 1, host: '127.0.0.1', port: echoPort });
const priv = await h2.wait((m) => m.rid === 1);
check('private address refused by default', !priv.ok && priv.code === 'EDENIED', JSON.stringify(priv));
h2.send({ t: 'connect', rid: 2, host: 'example.org', port: 80 });
const den = await h2.wait((m) => m.rid === 2);
check('host outside allowHosts refused', !den.ok && den.code === 'EDENIED' && /not allowed/.test(den.message), JSON.stringify(den));
h2.p.stdin.end();
console.log(failed ? `${failed} FAILED` : 'all ok');
process.exit(failed ? 1 : 0);
