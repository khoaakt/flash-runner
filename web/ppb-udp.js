// PPB_UDPSocket;1.2 (what Flash's RTMFP uses): datagrams through the native helper, which has to have UDP switched on (`allowUDP`, see native/helper/udp.go).
// The backend is host.opts.sockets (web/socket-bridge.js): udpBind(host, port) -> { id, local }, udpSend(id, host, port, bytes) -> bytes sent,
// udpClose(id), and datagrams arrive through backend.onUdp({ t: 'data', id, ip, port, bytes } | { t: 'close', id }). Without a backend that has these
// (Node tests, the standalone viewer, a helper without UDP) every call fails the way a missing network does and Flash falls back or reports an error.
import { PP_OK, PP_OK_COMPLETIONPENDING, PP_ERROR_FAILED, PP_ERROR_ABORTED, PP_ERROR_BADARGUMENT, PP_ERROR_BADRESOURCE, PP_ERROR_INPROGRESS, PP_ERROR_NOTSUPPORTED, PP_ERROR_BLOCKS_MAIN_THREAD } from './pp-consts.js';
import { errnoToPP } from './ppb-sockets.js';

const MAX_QUEUED = 256;          // datagrams kept for a plugin that is not receiving; the oldest go first
const PP_ERROR_ADDRESS_IN_USE = -108;

export function buildUdp(host) {
  const impl = {};
  const backend = () => (host.opts.sockets && host.opts.sockets.udpBind ? host.opts.sockets : null);
  const byId = new Map();                                    // backend socket id -> resource object
  const sock = (r) => host.getRes(r, 'udpsocket');
  const addr = (r) => host.getRes(r, 'netaddress');
  const mkAddr = (instance, ip, port) => host.newRes({ type: 'netaddress', instance, ip, port });
  const loopOf = (tid) => host.threadLoop.get(tid) || 1;
  const code = (e) => (e && e.code === 'EADDRINUSE' ? PP_ERROR_ADDRESS_IN_USE : errnoToPP(e && e.code));
  let wired = false;
  const wire = () => {
    if (wired || !backend()) return; wired = true;
    backend().onUdp = (ev) => {
      const s = byId.get(ev.id); if (!s) return;
      if (ev.t === 'close') { byId.delete(ev.id); s.state = 'closed'; abortRecv(s); return; }
      if (s.recv) { deliver(s, ev); return; }
      s.queue.push(ev); if (s.queue.length > MAX_QUEUED) s.queue.shift();
    };
  };

  /** Complete the waiting RecvFrom with one datagram (truncated to the buffer, as UDP does). */
  function deliver(s, ev) {
    const r = s.recv; s.recv = null;
    const n = Math.min(r.num, ev.bytes.length);
    if (!s.recvLogged) { s.recvLogged = true; host.log(`[udp] first datagram received: ${ev.bytes.length} bytes from ${ev.ip}:${ev.port}`); }
    host.putBytes(r.buf, ev.bytes.subarray(0, n));
    if (r.addrPtr) host.w32(r.addrPtr, mkAddr(s.instance, ev.ip, ev.port));
    host.postTask(r.loop, r.cc.func, [r.cc.user, n >>> 0]);
  }
  function abortRecv(s) { const r = s.recv; if (r) { s.recv = null; host.postTask(r.loop, r.cc.func, [r.cc.user, PP_ERROR_ABORTED >>> 0]); } }
  function shut(s) {
    if (s.state === 'closed') return;
    const open = s.state === 'bound'; s.state = 'closed';
    if (s.id) { byId.delete(s.id); if (open) backend()?.udpClose(s.id); }
    abortRecv(s);
  }

  impl['PPB_UDPSocket;1.2'] = {
    Create(tid, instance) { return host.newRes({ type: 'udpsocket', instance, state: 'new', id: 0, local: null, queue: [], recv: null, onFree() { shut(this); } }); },
    IsUDPSocket(tid, r) { return sock(r) ? 1 : 0; },
    Bind(tid, r, a, cc) {
      const s = sock(r), ad = addr(a);
      if (!s) return host.complete(tid, cc, PP_ERROR_BADRESOURCE);
      if (!ad) return host.complete(tid, cc, PP_ERROR_BADARGUMENT);
      if (s.state !== 'new') return host.complete(tid, cc, PP_ERROR_FAILED);
      if (!backend()) return host.complete(tid, cc, PP_ERROR_NOTSUPPORTED);
      wire(); s.state = 'binding';
      return host.finishAsync(tid, cc, loopOf(tid), (done) => {
        backend().udpBind(ad.ip, ad.port).then((b) => {
          if (s.state === 'closed') { backend().udpClose(b.id); return done(PP_ERROR_ABORTED); }
          s.id = b.id; s.local = b.local; s.state = 'bound'; byId.set(b.id, s); host.log(`[udp] bound ${b.local.ip}:${b.local.port}`); done(PP_OK);
        }, (e) => { s.state = 'new'; done(code(e)); });
      });
    },
    GetBoundAddress(tid, r) { const s = sock(r); return s && s.state === 'bound' && s.local ? mkAddr(s.instance, s.local.ip, s.local.port) : 0; },
    RecvFrom(tid, r, buf, num, addrPtr, cc) {
      const s = sock(r);
      if (!s) return host.complete(tid, cc, PP_ERROR_BADRESOURCE);
      if (s.state !== 'bound') return host.complete(tid, cc, PP_ERROR_FAILED);
      if (!cc.func) return tid === host.mainTid ? PP_ERROR_BLOCKS_MAIN_THREAD : PP_ERROR_NOTSUPPORTED;   // only the callback form is supported
      if (s.recv) return PP_ERROR_INPROGRESS;
      s.recv = { buf, num, addrPtr, cc, loop: loopOf(tid) };
      const first = s.queue.shift();
      if (first) deliver(s, first);
      return PP_OK_COMPLETIONPENDING;
    },
    SendTo(tid, r, buf, num, a, cc) {
      const s = sock(r), ad = addr(a);
      if (!s) return host.complete(tid, cc, PP_ERROR_BADRESOURCE);
      if (!ad) return host.complete(tid, cc, PP_ERROR_BADARGUMENT);
      if (s.state !== 'bound') return host.complete(tid, cc, PP_ERROR_FAILED);
      const bytes = host.ro(buf, num).slice();
      if (!s.sentLogged) { s.sentLogged = true; host.log(`[udp] first datagram: ${num} bytes to ${ad.ip}:${ad.port}`); }
      return host.finishAsync(tid, cc, loopOf(tid), (done) => {
        backend().udpSend(s.id, ad.ip, ad.port, bytes).then((n) => done(n), (e) => done(code(e)));
      });
    },
    Close(tid, r) { const s = sock(r); if (s) shut(s); },
    SetOption(tid, r, name, value, cc) { return host.complete(tid, cc, sock(r) ? PP_OK : PP_ERROR_BADRESOURCE); },
    JoinGroup(tid, r, g, cc) { return host.complete(tid, cc, PP_ERROR_NOTSUPPORTED); },
    LeaveGroup(tid, r, g, cc) { return host.complete(tid, cc, PP_ERROR_NOTSUPPORTED); },
  };

  // ---- PPB_HostResolver;1.0: RTMFP resolves the server name before it sends anything ------------------------------------------------
  const hr = (r) => host.getRes(r, 'hostresolver');
  const isIp = (n) => /^\d+\.\d+\.\d+\.\d+$/.test(n) || /^\[?[0-9a-fA-F:]*:[0-9a-fA-F:.]*\]?$/.test(n);
  impl['PPB_HostResolver;1.0'] = {
    Create(tid, instance) { return host.newRes({ type: 'hostresolver', instance, name: '', port: 0, addrs: [] }); },
    IsHostResolver(tid, r) { return hr(r) ? 1 : 0; },
    Resolve(tid, r, namePtr, port, hint, cc) {
      const h = hr(r); if (!h) return host.complete(tid, cc, PP_ERROR_BADRESOURCE);
      const name = host.cstr(namePtr); h.name = name; h.port = port; h.addrs = [];
      if (isIp(name)) { h.addrs = [name.replace(/^\[|\]$/g, '')]; return host.complete(tid, cc, PP_OK); }
      if (!backend()?.resolve) return host.complete(tid, cc, PP_ERROR_NOTSUPPORTED);
      return host.finishAsync(tid, cc, loopOf(tid), (done) => {
        backend().resolve(name).then((a) => { h.addrs = a; done(a.length ? PP_OK : errnoToPP('ENOTFOUND')); }, (e) => done(errnoToPP(e && e.code)));
      });
    },
    GetCanonicalName(tid, r) { const h = hr(r); return host.mkVar(h && h.addrs.length ? h.name : undefined); },
    GetNetAddressCount(tid, r) { return hr(r)?.addrs.length ?? 0; },
    GetNetAddress(tid, r, i) { const h = hr(r); return h && i >= 0 && i < h.addrs.length ? mkAddr(h.instance, h.addrs[i], h.port) : 0; },
  };
  return impl;
}
