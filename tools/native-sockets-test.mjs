// State machine of extension/player/native-sockets.js against a fake chrome.runtime.connectNative (no browser):
// "helper not installed" must be reported once, answered instantly afterwards, retried only now and then, and withdrawn when the helper turns up.
//   node tools/native-sockets-test.mjs
import { NativeSockets } from '../extension/player/native-sockets.js';

let failed = 0;
const check = (n, ok, extra = '') => { console.log((ok ? 'PASS ' : 'FAIL ') + n + (ok ? '' : '  ' + extra)); if (!ok) failed++; };
const tick = () => new Promise((r) => setTimeout(r, 0));

let connects = 0, ports = [], hostError = 'Specified native messaging host not found.';
globalThis.chrome = { runtime: {
  lastError: undefined,
  connectNative() {
    connects++;
    const msg = [], dis = [];
    const port = { name: 'x', posted: [], onMessage: { addListener: (f) => msg.push(f) }, onDisconnect: { addListener: (f) => dis.push(f) },
      postMessage(m) { if (port.dead) throw new Error('Attempting to use a disconnected port object'); port.posted.push(m); },
      disconnect() { port.dead = true; },
      fromHelper: (m) => msg.forEach((f) => f(m)),
      die(why) { port.dead = true; globalThis.chrome.runtime.lastError = why ? { message: why } : undefined; dis.forEach((f) => f()); globalThis.chrome.runtime.lastError = undefined; } };
    ports.push(port);
    if (hostError) queueMicrotask(() => port.die(hostError));    // what Chrome does for an unregistered host: the port disconnects right away
    return port;
  },
} };

const make = () => { const got = [], states = [], problems = [], s = new NativeSockets((m) => got.push(m), (t) => problems.push(t), null, (st) => states.push(st)); return { s, got, states, problems }; };

{ // 1. not installed
  const { s, got, states, problems } = make();
  s.send({ t: 'connect', rid: 1, host: 'h', port: 5000 });
  await tick();
  check('missing host: the pending connect is answered EHELPER', got.length === 1 && got[0].rid === 1 && got[0].code === 'EHELPER', JSON.stringify(got));
  check('missing host: reported once as missing', states.join() === 'missing', states.join());
  check('missing host: the console text points at the settings, not at a script', problems.length === 1 && /extension settings/.test(problems[0]) && !/install-native-host/.test(problems[0]), problems.join('|'));
  const before = connects;
  s.send({ t: 'connect', rid: 2, host: 'h', port: 5000 });
  check('retries within the window are answered at once without another connectNative', got.length === 2 && got[1].rid === 2 && got[1].code === 'EHELPER' && connects === before, `${got.length} ${connects} ${before}`);
  check('... and are not reported again', states.join() === 'missing');
  // 2. window passes, still missing: tries again, stays quiet
  const realNow = Date.now; Date.now = () => realNow() + 6000;
  s.send({ t: 'connect', rid: 3, host: 'h', port: 5000 });
  await tick();
  check('after the retry window connectNative is tried again', connects === before + 1, `${connects} vs ${before}`);
  check('... still missing: no second notification', states.join() === 'missing', states.join());
  // 3. the helper has been installed meanwhile
  hostError = null;
  Date.now = () => realNow() + 12000;
  s.send({ t: 'connect', rid: 4, host: 'h', port: 5000 });
  await tick();
  const port = ports.at(-1);
  check('helper present: the request reaches it', port.posted.length === 1 && port.posted[0].rid === 4, JSON.stringify(port.posted));
  port.fromHelper({ rid: 4, ok: true, id: 1, remote: { ip: '1.2.3.4', port: 5000 }, local: { ip: '127.0.0.1', port: 1 } });
  check('helper answered: the notice is withdrawn', states.join() === 'missing,ok', states.join());
  port.fromHelper({ t: 'data', id: 1, data: 'AA==' });
  check('later messages do not toggle anything', states.join() === 'missing,ok');
  // 4. a helper that dies is not "not installed"
  port.die('Native host has exited.');
  check('helper exiting is a problem but not "missing"', states.join() === 'missing,ok' && s.missing === false);
  Date.now = realNow;
}

{ // forbidden / failed to start count as not installed (a broken registration needs the installer too)
  for (const why of ['Access to the specified native messaging host is forbidden.', 'Failed to start native messaging host.']) {
    hostError = why;
    const { s, states } = make();
    s.send({ t: 'connect', rid: 1, host: 'h', port: 5000 });
    await tick();
    check(`"${why}" is reported as missing`, states.join() === 'missing', states.join());
  }
  hostError = 'Native host has exited.';
  const { s, states } = make();
  s.send({ t: 'connect', rid: 1, host: 'h', port: 5000 });
  await tick();
  check('"Native host has exited." is not reported as missing', states.length === 0, states.join());
}

{ // Firefox: the frame cannot open a native port, so it talks to the background relay (extension/background.js) over runtime.connect
  hostError = null;
  const before = connects, relays = [];
  globalThis.chrome.runtime.getManifest = () => ({ browser_specific_settings: { gecko: { id: 'x@y' } } });
  globalThis.chrome.runtime.connect = (info) => {
    const msg = [], dis = [], port = { info, posted: [], onMessage: { addListener: (f) => msg.push(f) }, onDisconnect: { addListener: (f) => dis.push(f) },
      postMessage: (m) => port.posted.push(m), disconnect() {}, fromRelay: (m) => msg.forEach((f) => f(m)), die: () => dis.forEach((f) => f({ error: null })) };
    relays.push(port); return port;
  };
  const { s, got, states } = make();
  s.send({ t: 'connect', rid: 1, host: 'h', port: 5000 });
  await tick();
  check('Firefox: the socket client opens the relay port, not a native port', relays.length === 1 && relays[0].info.name === 'flash-runner:native-sockets' && connects === before, `${relays.length} ${connects} ${before}`);
  check('Firefox: requests pass through the relay unchanged', relays[0].posted.length === 1 && relays[0].posted[0].t === 'connect' && relays[0].posted[0].rid === 1, JSON.stringify(relays[0].posted));
  relays[0].fromRelay({ rid: 1, ok: true, id: 7, remote: { ip: '1.2.3.4', port: 5000 } });
  check('Firefox: the helper\'s reply reaches the worker', got.length === 1 && got[0].rid === 1 && got[0].ok === true, JSON.stringify(got));
  relays[0].fromRelay({ relayClosed: 'No such native application com.flashrunner.sockets' });
  relays[0].die();
  await tick();
  check('Firefox: the relay explaining that the host is not registered is reported as missing', states.join() === 'missing', states.join());
  delete globalThis.chrome.runtime.getManifest; delete globalThis.chrome.runtime.connect;
}

console.log(failed ? `${failed} FAILED` : 'all ok');
process.exit(failed ? 1 : 0);
