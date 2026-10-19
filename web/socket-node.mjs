// Socket backend for Node (tests): real TCP through node:net, same interface as the extension's native-helper backend.
import net from 'node:net';
import tls from 'node:tls';

export class NodeSockets {
  constructor() { this.map = new Map(); this.next = 1; this.onEvent = () => {}; }
  connect(host, port) {
    return new Promise((resolve, reject) => {
      const s = net.connect({ host, port });
      const id = this.next++;
      s.once('error', (e) => reject({ code: e.code || 'ECONNREFUSED' }));
      s.once('connect', () => {
        s.removeAllListeners('error');
        this.map.set(id, s);
        s.on('data', (d) => this.onEvent({ t: 'data', id, bytes: new Uint8Array(d) }));
        s.on('close', () => this.onEvent({ t: 'close', id }));
        s.on('error', (e) => this.onEvent({ t: 'close', id, error: e.code }));
        resolve({ id, local: { ip: s.localAddress, port: s.localPort }, remote: { ip: s.remoteAddress, port: s.remotePort } });
      });
    });
  }
  write(id, bytes) { return new Promise((res, rej) => { const s = this.map.get(id); if (!s) return rej({ code: 'EPIPE' }); s.write(Buffer.from(bytes), (e) => (e ? rej({ code: e.code }) : res(bytes.length))); }); }
  close(id) { const s = this.map.get(id); if (s) { this.map.delete(id); s.destroy(); } }
  pause(id) { this.map.get(id)?.pause(); }
  resume(id) { this.map.get(id)?.resume(); }
  tls(id, servername) {
    return new Promise((res, rej) => {
      const raw = this.map.get(id); if (!raw) return rej({ code: 'EPIPE' });
      raw.removeAllListeners('data');
      const t = tls.connect({ socket: raw, servername, rejectUnauthorized: true });
      t.once('secureConnect', () => { this.map.set(id, t); t.on('data', (d) => this.onEvent({ t: 'data', id, bytes: new Uint8Array(d) })); res(); });
      t.once('error', (e) => rej({ code: e.code || 'ECONNRESET' }));
    });
  }
}
