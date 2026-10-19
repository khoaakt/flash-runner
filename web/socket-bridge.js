// Socket backend used inside the emulator worker in the browser: requests go to the page (player.js), which talks to the native
// helper over Chrome's native messaging. Replies and unsolicited events come back through handle().
export class BridgeSockets {
  constructor(post) { this.post = post; this.seq = 0; this.wait = new Map(); this.onEvent = () => {}; }
  _req(msg) { return new Promise((res, rej) => { const rid = ++this.seq; this.wait.set(rid, { res, rej }); this.post({ ...msg, rid }); }); }
  handle(m) {
    if (m.rid) {
      const w = this.wait.get(m.rid); this.wait.delete(m.rid); if (!w) return;
      m.ok ? w.res(m) : w.rej({ code: m.code || 'ECONNREFUSED', message: m.message });
    } else if (m.t === 'data') this.onEvent({ t: 'data', id: m.id, bytes: m.bytes });
    else if (m.t === 'close') this.onEvent({ t: 'close', id: m.id, error: m.error });
  }
  connect(host, port) { return this._req({ t: 'connect', host, port }).then((r) => ({ id: r.id, local: r.local, remote: r.remote })); }
  write(id, bytes) { return this._req({ t: 'write', id, bytes }).then(() => bytes.length); }
  close(id) { this.post({ t: 'close', id }); }
  pause(id) { this.post({ t: 'pause', id }); }
  resume(id) { this.post({ t: 'resume', id }); }
  tls(id, name) { return this._req({ t: 'tls', id, name }); }
}
