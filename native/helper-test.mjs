// Drives the socket helper the way Chrome does (native messaging framing on stdin/stdout) against an echo server, without a browser.
//   node native/helper-test.mjs [path/to/flash-helper]        (default: native/out/flash-helper-<this platform>)
import { spawn, spawnSync, execFileSync } from 'node:child_process';
import net from 'node:net';
import { mkdtempSync, writeFileSync, readFileSync } from 'node:fs';
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

function start(cfg, args = [ORIGIN]) {
  const dir = mkdtempSync(join(tmpdir(), 'flash-helper-test-')), cfgFile = join(dir, 'sockets.json');
  writeFileSync(cfgFile, JSON.stringify(cfg));
  const p = spawn(bin, args, { env: { ...process.env, FLASHRUNNER_SOCKETS_CONFIG: cfgFile }, stdio: ['pipe', 'pipe', 'inherit'] });
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
// Firefox starts a native host with <path of the host manifest> <add-on id> and enforces allowed_extensions itself
const FF_ID = JSON.parse(readFileSync(join(root, 'tools/firefox/manifest-overlay.json'), 'utf8')).set.browser_specific_settings.gecko.id;
// each browser family gets its own manifest: Firefox's schema rejects the Chromium key
const mf = JSON.parse(execFileSync(bin, ['manifest', '/x/flash-helper'], { encoding: 'utf8' }));
check('Chromium manifest names the extension origin and nothing else', mf.allowed_origins?.[0] === ORIGIN && mf.allowed_origins.length === 1 && !('allowed_extensions' in mf), JSON.stringify(mf));
const mff = JSON.parse(execFileSync(bin, ['manifest', '/x/flash-helper', '--firefox'], { encoding: 'utf8' }));
check('Firefox manifest names the add-on id of the build overlay and nothing else', mff.allowed_extensions?.[0] === FF_ID && mff.allowed_extensions.length === 1 && !('allowed_origins' in mff) && mff.type === 'stdio' && mff.name === mf.name, JSON.stringify(mff));
const ff = start({}, [join(tmpdir(), 'com.flashrunner.sockets.json'), FF_ID]);
ff.send({ t: 'hello', rid: 1 });
const ffHello = await ff.wait((m) => m.rid === 1);
check('hello when started the way Firefox starts it', ffHello.ok && ffHello.version === 2, JSON.stringify(ffHello));
ff.p.stdin.end();
const stranger = spawnSync(bin, [join(tmpdir(), 'com.flashrunner.sockets.json'), 'someone-else@example.com'], { input: '', timeout: 5000 });
check('an unknown add-on id does not start the helper', stranger.status === 2, 'exit ' + stranger.status);

// ---- UDP (RTMFP): off by default, then bind / send / reply / unsolicited datagrams / close
{
  const dgram = await import('node:dgram');
  const off = start({ allowPrivate: true });
  off.send({ t: 'hello', rid: 1 }); const offHello = await off.wait((m) => m.rid === 1);
  off.send({ t: 'udp-bind', rid: 2, port: 0 }); const offBind = await off.wait((m) => m.rid === 2);
  check('UDP is off by default (hello says so, bind is refused)', offHello.udp === false && !offBind.ok && offBind.code === 'EDENIED' && /allowUDP/.test(offBind.message), JSON.stringify([offHello, offBind]));
  off.p.stdin.end();

  const server = dgram.createSocket('udp4'), seen = [];
  server.on('message', (msg, rinfo) => { seen.push({ msg, rinfo }); if (msg.toString() === 'ping') server.send(Buffer.from('pong'), rinfo.port, rinfo.address); });
  await new Promise((r) => server.bind(0, '127.0.0.1', r));
  const sport = server.address().port;
  const u = start({ allowPrivate: true, allowUDP: true });
  u.send({ t: 'hello', rid: 1 }); const uHello = await u.wait((m) => m.rid === 1);
  check('hello advertises UDP when enabled', uHello.udp === true, JSON.stringify(uHello));
  u.send({ t: 'udp-bind', rid: 2, port: 0 }); const bound = await u.wait((m) => m.rid === 2);
  check('udp-bind gives an id and the bound port', bound.ok && bound.id > 0 && bound.local.port > 0, JSON.stringify(bound));
  u.send({ t: 'udp-send', rid: 3, id: bound.id, host: '127.0.0.1', port: sport, data: Buffer.from('ping').toString('base64') });
  const sent = await u.wait((m) => m.rid === 3);
  const reply = await u.wait((m) => m.t === 'udp-data' && m.id === bound.id);
  check('a datagram reaches the server from the bound port and the reply comes back', sent.ok && sent.n === 4 && seen.length === 1 && seen[0].rinfo.port === bound.local.port && Buffer.from(reply.data, 'base64').toString() === 'pong' && reply.port === sport, JSON.stringify({ sent, reply, seen: seen.length }));
  // a stranger that was never sent to must not get through
  const stranger = dgram.createSocket('udp4'); let strangerBound = true;
  await new Promise((r) => { stranger.once('error', () => { strangerBound = false; r(); }); stranger.bind(0, '127.0.0.2', r); });   // 127.0.0.2 is another loopback address on Linux and Windows, absent on macOS
  const before = u.events.length;
  if (strangerBound) stranger.send(Buffer.from('intruder'), bound.local.port, '127.0.0.1');
  await new Promise((r) => setTimeout(r, 400));
  if (!strangerBound) console.log('SKIP a datagram from an address nobody was sent to is dropped (this OS has no 127.0.0.2)'); else check('a datagram from an address nobody was sent to is dropped', !u.events.slice(before).some((m) => m.t === 'udp-data' && Buffer.from(m.data, 'base64').toString() === 'intruder'), JSON.stringify(u.events.slice(before)));
  stranger.close();
  u.send({ t: 'udp-send', rid: 4, id: bound.id, host: '127.0.0.1', port: sport, data: Buffer.alloc(70000).toString('base64') });
  const big2 = await u.wait((m) => m.rid === 4);
  check('an oversized datagram is refused', !big2.ok && big2.code === 'EINVAL', JSON.stringify(big2));
  u.send({ t: 'udp-close', id: bound.id });
  await new Promise((r) => setTimeout(r, 200));
  u.send({ t: 'udp-send', rid: 5, id: bound.id, host: '127.0.0.1', port: sport, data: Buffer.from('x').toString('base64') });
  const closed = await u.wait((m) => m.rid === 5);
  check('sending on a closed UDP socket fails', !closed.ok && closed.code === 'EPIPE', JSON.stringify(closed));
  u.p.stdin.end();
  // public addresses only unless allowPrivate
  const pub = start({ allowUDP: true });
  pub.send({ t: 'udp-bind', rid: 1, port: 0 }); const pb = await pub.wait((m) => m.rid === 1);
  pub.send({ t: 'udp-send', rid: 2, id: pb.id, host: '127.0.0.1', port: sport, data: Buffer.from('x').toString('base64') });
  const denied = await pub.wait((m) => m.rid === 2);
  check('UDP to a private address is refused by default', !denied.ok && denied.code === 'EDENIED', JSON.stringify(denied));
  pub.p.stdin.end();
  server.close();
}

console.log(failed ? `${failed} FAILED` : 'all ok');
process.exit(failed ? 1 : 0);
