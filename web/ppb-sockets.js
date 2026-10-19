// PPB_TCPSocket_Private (what Flash's flash.net.Socket / XMLSocket and socket policy files use) and PPB_NetAddress_Private.
//
// A browser cannot open TCP connections, so the actual sockets are provided by a backend (host.opts.sockets):
//   connect(host, port) -> Promise<{ id, local: {ip, port}, remote: {ip, port} }>   rejects with { code } (see ERRNO)
//   write(id, Uint8Array) -> Promise<number>      close(id)      tls(id, serverName) -> Promise
//   onEvent = (ev) => ...  set by this file; ev = { t: 'data', id, bytes } | { t: 'close', id, error? }
// In the extension the backend talks to a small native-messaging helper (native/helper, Go); in Node tests it is
// web/socket-node.mjs. Without a backend every connect fails with PP_ERROR_NOTSUPPORTED, which is what Flash sees today.
import { PP_OK, PP_ERROR_FAILED, PP_ERROR_ABORTED, PP_ERROR_BADARGUMENT, PP_ERROR_BADRESOURCE, PP_ERROR_INPROGRESS, PP_ERROR_NOTSUPPORTED, PP_ERROR_CONNECTION_FAILED } from './pp-consts.js';

const PP_ERROR_CONNECTION_CLOSED = -100, PP_ERROR_CONNECTION_RESET = -101, PP_ERROR_CONNECTION_REFUSED = -102, PP_ERROR_CONNECTION_TIMEDOUT = -105,
  PP_ERROR_ADDRESS_UNREACHABLE = -107, PP_ERROR_NAME_NOT_RESOLVED = -110, PP_ERROR_NOACCESS = -7;
const ERRNO = { ECONNREFUSED: PP_ERROR_CONNECTION_REFUSED, ECONNRESET: PP_ERROR_CONNECTION_RESET, ETIMEDOUT: PP_ERROR_CONNECTION_TIMEDOUT, ENOTFOUND: PP_ERROR_NAME_NOT_RESOLVED,
  EAI_AGAIN: PP_ERROR_NAME_NOT_RESOLVED, EHOSTUNREACH: PP_ERROR_ADDRESS_UNREACHABLE, ENETUNREACH: PP_ERROR_ADDRESS_UNREACHABLE, EPIPE: PP_ERROR_CONNECTION_CLOSED, EACCES: PP_ERROR_NOACCESS, EDENIED: PP_ERROR_NOACCESS };
export const errnoToPP = (code) => ERRNO[code] ?? PP_ERROR_CONNECTION_FAILED;

const MAX_READ = 1 << 20;                                  // PPAPI caps one Read/Write at 1 MiB
const MAX_BUFFERED = 8 << 20;                              // unread bytes kept per socket before the backend is told to pause

// ---- PP_NetAddress_Private { uint32 size; char data[128] }: our own layout, only ever read back by us ---------------------------
//   data[0] = family (1 = IPv4, 2 = IPv6), data[2..3] = port (big endian), IPv4: data[4..7]; IPv6: data[4..19] + scope id data[20..23]
const ADDR_SIZE = 132;
function parseIp(ip) {
  if (/^\d+\.\d+\.\d+\.\d+$/.test(ip)) return { v: 4, b: ip.split('.').map(Number) };
  const m = ip.replace(/^\[|\]$/g, '').split('%')[0];
  const groups = (s) => (s ? s.split(':').flatMap((g) => {
    if (g.includes('.')) { const q = g.split('.').map(Number); return [(q[0] << 8) | q[1], (q[2] << 8) | q[3]]; }   // embedded IPv4 tail (::ffff:1.2.3.4)
    return [parseInt(g || '0', 16)];
  }) : []);
  const halves = m.split('::');
  const head = groups(halves[0]), tail = halves.length > 1 ? groups(halves[1]) : [];
  const all = [...head, ...new Array(Math.max(0, 8 - head.length - tail.length)).fill(0), ...tail].slice(0, 8);
  return { v: 6, b: all.flatMap((g) => [(g >> 8) & 255, g & 255]) };
}
const fmtIp = (v, b) => (v === 4 ? b.join('.') : Array.from({ length: 8 }, (_, i) => ((b[i * 2] << 8) | b[i * 2 + 1]).toString(16)).join(':'));

export function buildSockets(host) {
  const impl = {};
  const backend = () => host.opts.sockets;
  const sockById = new Map();                              // backend socket id -> resource
  let wired = false;
  const wire = () => {
    if (wired || !backend()) return;
    wired = true;
    backend().onEvent = (ev) => {
      const s = sockById.get(ev.id); if (!s) return;
      if (ev.t === 'data') {
        s.chunks.push(ev.bytes); s.buffered += ev.bytes.length;
        if (s.buffered > MAX_BUFFERED && !s.paused && backend().pause) { s.paused = true; backend().pause(s.id); }
        serve(s);
      } else if (ev.t === 'close') { s.eof = true; s.error = ev.error ? errnoToPP(ev.error) : 0; serve(s); }
    };
  };

  const sock = (r) => host.getRes(r, 'tcpsocket');
  const writeAddr = (p, a) => {
    if (!a) return 0;
    const ip = parseIp(a.ip);
    const b = new Uint8Array(ADDR_SIZE);
    new DataView(b.buffer).setUint32(0, ip.v === 4 ? 8 : 24, true);
    b[4] = ip.v === 4 ? 1 : 2; b[6] = (a.port >> 8) & 255; b[7] = a.port & 255; b.set(ip.b, 8);
    host.putBytes(p, b);
    return 1;
  };
  const readAddr = (p) => {
    if (!p) return null;
    const d = p + 4, fam = host.ro(d, 1)[0];
    if (fam !== 1 && fam !== 2) return null;
    const port = (host.ro(d + 2, 1)[0] << 8) | host.ro(d + 3, 1)[0];
    const b = Array.from(host.ro(d + 4, fam === 1 ? 4 : 16));
    return { ip: fmtIp(fam === 1 ? 4 : 6, b), port };
  };

  /** Hand buffered bytes (or EOF / an error) to a waiting Read. */
  function serve(s) {
    const r = s.pending; if (!r) return;
    if (s.chunks.length) {
      const n = Math.min(r.n, s.buffered);
      const dst = host.bytesW(r.ptr, n); let off = 0;
      while (off < n) {
        const c = s.chunks[0], take = Math.min(c.length, n - off);
        dst.set(c.subarray(0, take), off); off += take;
        if (take === c.length) s.chunks.shift(); else s.chunks[0] = c.subarray(take);
      }
      s.buffered -= n; s.pending = null;
      if (s.paused && s.buffered < MAX_BUFFERED / 2 && backend()?.resume) { s.paused = false; backend().resume(s.id); }
      r.done(n);
    } else if (s.eof || s.state === 'closed') { s.pending = null; r.done(s.error || (s.state === 'closed' && !s.eof ? PP_ERROR_ABORTED : 0)); }
  }

  const shut = (s) => {
    if (s.id != null) { backend()?.close?.(s.id); sockById.delete(s.id); }
    s.state = 'closed'; s.eof = true;
    if (s.pending) serve(s);
  };

  impl.PPB_TCPSocket_Private = {
    Create(tid, instance) {
      return host.newRes({ type: 'tcpsocket', instance, state: 'new', id: null, chunks: [], buffered: 0, eof: false, error: 0, pending: null, writing: false, paused: false, local: null, remote: null, onFree() { shut(this); } });
    },
    IsTCPSocket(tid, r) { return sock(r) ? 1 : 0; },
    Connect(tid, r, hostPtr, port, cc) {
      const s = sock(r); if (!s) return host.complete(tid, cc, PP_ERROR_BADRESOURCE);
      if (s.state !== 'new') return host.complete(tid, cc, PP_ERROR_FAILED);
      if (!backend()) return host.complete(tid, cc, PP_ERROR_NOTSUPPORTED);
      wire();
      const name = host.cstr(hostPtr);
      return connectTo(tid, s, name, port & 0xffff, cc);
    },
    ConnectWithNetAddress(tid, r, addrPtr, cc) {
      const s = sock(r); if (!s) return host.complete(tid, cc, PP_ERROR_BADRESOURCE);
      if (s.state !== 'new') return host.complete(tid, cc, PP_ERROR_FAILED);
      if (!backend()) return host.complete(tid, cc, PP_ERROR_NOTSUPPORTED);
      wire();
      const a = readAddr(addrPtr); if (!a) return host.complete(tid, cc, PP_ERROR_BADARGUMENT);
      return connectTo(tid, s, a.ip, a.port, cc);
    },
    GetLocalAddress(tid, r, p) { const s = sock(r); return s && s.state === 'connected' && s.local ? writeAddr(p, s.local) : 0; },
    GetRemoteAddress(tid, r, p) { const s = sock(r); return s && s.state === 'connected' && s.remote ? writeAddr(p, s.remote) : 0; },
    SSLHandshake(tid, r, namePtr, port, cc) {
      const s = sock(r); if (!s || s.state !== 'connected') return host.complete(tid, cc, PP_ERROR_FAILED);
      if (!backend()?.tls) return host.complete(tid, cc, PP_ERROR_NOTSUPPORTED);
      const name = host.cstr(namePtr);
      return host.finishAsync(tid, cc, host.threadLoop.get(tid) || 1, (done) => {
        backend().tls(s.id, name).then(() => done(PP_OK), (e) => done(errnoToPP(e && e.code)));
      });
    },
    GetServerCertificate() { return 0; },
    AddChainBuildingCertificate() { return 0; },
    Read(tid, r, buf, n, cc) {
      const s = sock(r); if (!s) return host.complete(tid, cc, PP_ERROR_BADRESOURCE);
      if (s.state === 'new' || s.state === 'connecting') return host.complete(tid, cc, PP_ERROR_FAILED);
      if (n <= 0) return host.complete(tid, cc, PP_ERROR_BADARGUMENT);
      if (s.pending) return host.complete(tid, cc, PP_ERROR_INPROGRESS);
      n = Math.min(n, MAX_READ);
      return host.finishAsync(tid, cc, host.threadLoop.get(tid) || 1, (done) => { s.pending = { ptr: buf, n, done }; serve(s); });
    },
    Write(tid, r, buf, n, cc) {
      const s = sock(r); if (!s) return host.complete(tid, cc, PP_ERROR_BADRESOURCE);
      if (s.state !== 'connected') return host.complete(tid, cc, s.state === 'closed' ? PP_ERROR_CONNECTION_CLOSED : PP_ERROR_FAILED);
      if (n <= 0) return host.complete(tid, cc, PP_ERROR_BADARGUMENT);
      if (s.writing) return host.complete(tid, cc, PP_ERROR_INPROGRESS);
      n = Math.min(n, MAX_READ);
      const bytes = host.ro(buf, n).slice();
      s.writing = true;
      return host.finishAsync(tid, cc, host.threadLoop.get(tid) || 1, (done) => {
        backend().write(s.id, bytes).then(() => { s.writing = false; done(n); }, (e) => { s.writing = false; done(errnoToPP(e && e.code)); });
      });
    },
    Disconnect(tid, r) { const s = sock(r); if (s) shut(s); },
    SetOption(tid, r, name, value, cc) { return host.complete(tid, cc, sock(r) ? PP_OK : PP_ERROR_BADRESOURCE); },
  };

  // ---- the public (resource based) variants: PPB_NetAddress;1.0 and PPB_TCPSocket;1.x ----------------------------------------------
  const addrRes = (r) => host.getRes(r, 'netaddress');
  const mkAddr = (instance, a) => host.newRes({ type: 'netaddress', instance, ip: a.ip, port: a.port });
  impl.PPB_NetAddress = {
    IsNetAddress(tid, r) { return addrRes(r) ? 1 : 0; },
    CreateFromIPv4Address(tid, instance, p) { return mkAddr(instance, { ip: Array.from(host.ro(p + 2, 4)).join('.'), port: (host.bytes(p, 1)[0] << 8) | host.bytes(p + 1, 1)[0] }); },
    CreateFromIPv6Address(tid, instance, p) { return mkAddr(instance, { ip: fmtIp(6, Array.from(host.ro(p + 2, 16))), port: (host.bytes(p, 1)[0] << 8) | host.bytes(p + 1, 1)[0] }); },
    GetFamily(tid, r) { const a = addrRes(r); return a ? parseIp(a.ip).v === 4 ? 1 : 2 : 0; },
    DescribeAsString(tid, r, includePort) { const a = addrRes(r); if (!a) return host.mkVar(undefined); const v6 = parseIp(a.ip).v === 6; return host.mkVar(includePort ? (v6 ? `[${a.ip}]:${a.port}` : `${a.ip}:${a.port}`) : a.ip); },
    DescribeAsIPv4Address(tid, r, out) { const a = addrRes(r); if (!a) return 0; const ip = parseIp(a.ip); if (ip.v !== 4) return 0; host.bytes(out, 1)[0] = (a.port >> 8) & 255; host.bytes(out + 1, 1)[0] = a.port & 255; host.bytes(out + 2, 4).set(ip.b); return 1; },
    DescribeAsIPv6Address(tid, r, out) { const a = addrRes(r); if (!a) return 0; const ip = parseIp(a.ip); if (ip.v !== 6) return 0; host.bytes(out, 1)[0] = (a.port >> 8) & 255; host.bytes(out + 1, 1)[0] = a.port & 255; host.bytes(out + 2, 16).set(ip.b); return 1; },
  };
  impl.PPB_TCPSocket = {
    ...impl.PPB_TCPSocket_Private,
    Bind(tid, r, a, cc) { return host.complete(tid, cc, sock(r) ? PP_OK : PP_ERROR_BADRESOURCE); },
    Connect(tid, r, a, cc) {
      const s = sock(r); if (!s) return host.complete(tid, cc, PP_ERROR_BADRESOURCE);
      const ad = addrRes(a); if (!ad) return host.complete(tid, cc, PP_ERROR_BADARGUMENT);
      if (s.state !== 'new') return host.complete(tid, cc, PP_ERROR_FAILED);
      if (!backend()) return host.complete(tid, cc, PP_ERROR_NOTSUPPORTED);
      wire();
      return connectTo(tid, s, ad.ip, ad.port, cc);
    },
    GetLocalAddress(tid, r) { const s = sock(r); return s && s.state === 'connected' && s.local ? mkAddr(s.instance, s.local) : 0; },
    GetRemoteAddress(tid, r) { const s = sock(r); return s && s.state === 'connected' && s.remote ? mkAddr(s.instance, s.remote) : 0; },
    Listen(tid, r, backlog, cc) { return host.complete(tid, cc, PP_ERROR_NOTSUPPORTED); },
    Accept(tid, r, out, cc) { return host.complete(tid, cc, PP_ERROR_NOTSUPPORTED); },
    Close(tid, r) { const s = sock(r); if (s) shut(s); },
  };

  function connectTo(tid, s, name, port, cc) {
    s.state = 'connecting';
    return host.finishAsync(tid, cc, host.threadLoop.get(tid) || 1, (done) => {
      backend().connect(name, port).then((c) => {
        if (s.state === 'closed') { backend().close?.(c.id); return done(PP_ERROR_ABORTED); }
        s.id = c.id; s.local = c.local; s.remote = c.remote; s.state = 'connected'; sockById.set(c.id, s);
        done(PP_OK);
      }, (e) => { s.state = 'new'; done(errnoToPP(e && e.code)); });
    });
  }

  // ---- PPB_NetAddress_Private --------------------------------------------------------------------------------------------------
  const fam = (p) => host.ro(p + 4, 1)[0];
  const portOf = (p) => (host.ro(p + 6, 1)[0] << 8) | host.bytes(p + 7, 1)[0];
  const ipOf = (p) => { const f = fam(p); return { v: f === 1 ? 4 : 6, b: Array.from(host.ro(p + 8, f === 1 ? 4 : 16)) }; };
  impl.PPB_NetAddress_Private = {
    AreEqual(tid, a, b) { const n = host.u32(a); return n === host.u32(b) && host.bytes(a + 4, n).every((x, i) => x === host.bytes(b + 4, n)[i]) ? 1 : 0; },
    AreHostsEqual(tid, a, b) { if (fam(a) !== fam(b)) return 0; const x = ipOf(a), y = ipOf(b); return x.b.every((v, i) => v === y.b[i]) ? 1 : 0; },
    Describe(tid, module, a, includePort) {
      const f = fam(a); if (f !== 1 && f !== 2) return host.mkVar(undefined);
      const ip = ipOf(a), s = fmtIp(ip.v, ip.b);
      return host.mkVar(includePort ? (ip.v === 6 ? `[${s}]:${portOf(a)}` : `${s}:${portOf(a)}`) : s);
    },
    ReplacePort(tid, src, port, out) { host.bytes(out, ADDR_SIZE).set(host.bytes(src, ADDR_SIZE)); host.bytes(out + 6, 1)[0] = (port >> 8) & 255; host.bytes(out + 7, 1)[0] = port & 255; return 1; },
    GetAnyAddress(tid, v6, out) { host.bytes(out, ADDR_SIZE).fill(0); host.w32(out, v6 ? 24 : 8); host.bytes(out + 4, 1)[0] = v6 ? 2 : 1; },
    GetFamily(tid, a) { const f = fam(a); return f === 1 || f === 2 ? f : 0; },
    GetPort(tid, a) { return portOf(a); },
    GetAddress(tid, a, dst, size) { const ip = ipOf(a); if (size < ip.b.length) return 0; host.bytes(dst, ip.b.length).set(ip.b); return 1; },
    GetScopeID() { return 0; },
    CreateFromIPv4Address(tid, ipPtr, port, out) { writeAddr(out, { ip: Array.from(host.ro(ipPtr, 4)).join('.'), port }); },
    CreateFromIPv6Address(tid, ipPtr, scope, port, out) { writeAddr(out, { ip: fmtIp(6, Array.from(host.ro(ipPtr, 16))), port }); },
  };
  return impl;
}
