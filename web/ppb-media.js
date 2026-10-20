// PPB_AudioInput(Dev) and PPB_VideoCapture(Dev) (flash.media.Microphone / Camera) on top of the player frame's getUserMedia (frame-media.js).
//
// Both are default-off: without the `mediaSites` permission for the embedding site the frame cannot open a device, enumeration finds nothing
// useful and Flash reports no camera or microphone.
//   audio  the frame streams mono Float32 at the rate of Flash's audio config; here it is cut into config.frames-sample blocks of 16-bit mono PCM
//          (what Chrome hands a plugin) and each block calls the plugin's audio_input_callback(buffer, bytes, latency, user_data).
//   video  the frame streams RGBA frames; each one is converted to planar I420 into a free PPB_Buffer and announced with
//          PPP_VideoCapture(Dev);0.1 OnBufferReady until the plugin gives the buffer back with ReuseBuffer.
import { PP_OK, PP_ERROR_FAILED, PP_ERROR_BADRESOURCE, PP_ERROR_NOACCESS, PP_ERROR_BLOCKS_MAIN_THREAD } from './pp-consts.js';

const DEV_AUDIO_CAPTURE = 1, DEV_VIDEO_CAPTURE = 2;
const STATUS_STOPPED = 0, STATUS_STARTED = 2;
const MAX_BUFFERS = 8;

/** RGBA -> planar I420 (Y plane, then 2x2-subsampled U, then V), BT.601 limited range, into `out` (w*h*3/2 bytes). */
export function rgbaToI420(rgba, w, h, out) {
  const uOff = w * h, vOff = uOff + (w >> 1) * (h >> 1);
  for (let y = 0; y < h; y++) {
    for (let x = 0; x < w; x++) {
      const i = (y * w + x) * 4, r = rgba[i], g = rgba[i + 1], b = rgba[i + 2];
      out[y * w + x] = ((66 * r + 129 * g + 25 * b + 128) >> 8) + 16;
      if (!(y & 1) && !(x & 1)) {
        const c = (y >> 1) * (w >> 1) + (x >> 1);
        out[uOff + c] = ((-38 * r - 74 * g + 112 * b + 128) >> 8) + 128;
        out[vOff + c] = ((112 * r - 94 * g - 18 * b + 128) >> 8) + 128;
      }
    }
  }
}

/** Float samples in [-1, 1] -> little-endian 16-bit PCM, written into `out` at `at` (Int16 index). */
export function floatToPcm16(samples, from, count, out, at) {
  for (let i = 0; i < count; i++) { const s = Math.max(-1, Math.min(1, samples[from + i])); out[at + i] = s < 0 ? s * 0x8000 : s * 0x7fff; }
}

export function buildMedia(host) {
  const impl = {};
  const u32le = (n) => new Uint8Array([n & 255, (n >> 8) & 255, (n >> 16) & 255, (n >> 24) & 255]);
  const loopOf = (tid) => host.threadLoop.get(tid) || 1;
  const service = (op, args) => (host.opts.service ? host.opts.service.request(op, args).then((r) => (r.ok ? r.value : Promise.reject(new Error(r.error)))) : Promise.reject(new Error('no media here')));
  const sessions = new Map();           // stream id -> resource object (audio input or video capture)
  let nextStream = 0;
  host.mediaIn = (m) => { const s = sessions.get(m.id); if (s) (m.kind === 'audio' ? audioIn : videoIn)(s, m); };

  const finish = (tid, cc, result) => host.complete(tid, cc, result);
  const failCode = (e) => (/denied|permission|allowed|NotAllowed|policy/i.test(String(e && e.message)) ? PP_ERROR_NOACCESS : PP_ERROR_FAILED);

  /** EnumerateDevices: ask the frame, make a DeviceRef per device of `kind`. */
  function enumerate(tid, r, out, cc, type, kind, label) {
    if (!cc.func && tid === host.mainTid) return PP_ERROR_BLOCKS_MAIN_THREAD;
    const run = service('media-devices', {}).then((devs) => devs.filter((d) => d.kind === kind), () => [])
      .then((devs) => host.arrayThenCallback(tid, out, 4, devs.map((d, i) => u32le(host.newRes({ type: 'deviceref', devType: type, name: d.label || `${label} ${i + 1}`, deviceId: d.id }))), cc));
    return cc.func ? PP_OK_PENDING : run;
  }
  const PP_OK_PENDING = -1;

  // ---- microphone -----------------------------------------------------------------------------------------
  const ai = (r) => host.getRes(r, 'audioinput');
  const aiClose = (a) => { if (a.stream != null) { sessions.delete(a.stream); service('media-close', { id: a.stream }).catch(() => {}); a.stream = null; } a.capturing = false; a.opened = false; };
  function audioIn(a, m) {
    if (!a.capturing || !a.cb) return;
    const f = new Float32Array(m.samples);
    a.queue.push(f); a.queued += f.length;
    while (a.queued >= a.frames && a.capturing) {
      const block = new Int16Array(a.frames); let at = 0;
      while (at < a.frames) {
        const head = a.queue[0], take = Math.min(head.length - a.queueOff, a.frames - at);
        floatToPcm16(head, a.queueOff, take, block, at); at += take; a.queueOff += take;
        if (a.queueOff >= head.length) { a.queue.shift(); a.queueOff = 0; }
      }
      a.queued -= a.frames;
      host.putBytes(a.bufPtr, new Uint8Array(block.buffer));
      const lat = new DataView(new ArrayBuffer(8)); lat.setFloat64(0, a.frames / a.rate, true);
      host.call(a.cb, a.latency ? [a.bufPtr, a.frames * 2, lat.getUint32(0, true), lat.getUint32(4, true), a.user]    // 0.4: callback(buffer, bytes, latency, user_data)
        : [a.bufPtr, a.frames * 2, a.user]);                                                                           // 0.3: callback(buffer, bytes, user_data)
    }
  }
  const audioInput = (latency) => ({
    Create(tid, instance) { return host.newRes({ type: 'audioinput', instance, latency: false, stream: null, opened: false, capturing: false, cb: 0, user: 0, queue: [], queued: 0, queueOff: 0, onFree() { aiClose(this); } }); },
    IsAudioInput(tid, r) { return ai(r) ? 1 : 0; },
    EnumerateDevices(tid, r, out, cc) { return ai(r) ? enumerate(tid, r, out, cc, DEV_AUDIO_CAPTURE, 'audioinput', 'Microphone') : PP_ERROR_BADRESOURCE; },
    MonitorDeviceChange() { return PP_OK; },
    Open(tid, r, deviceRef, config, cb, user, cc) {
      const a = ai(r), cfg = host.getRes(config, 'audioconfig');
      if (!a || !cfg || !cb) return finish(tid, cc, PP_ERROR_BADRESOURCE);
      const dev = host.getRes(deviceRef, 'deviceref');
      if (!cc.func && tid === host.mainTid) return PP_ERROR_BLOCKS_MAIN_THREAD;
      const id = ++nextStream;
      a.latency = latency;                              // the callback signature follows the version Open was called through (a resource may be created through another one)
      a.rate = cfg.rate; a.frames = cfg.frames; a.cb = cb; a.user = user; a.cfg = cfg; cfg.refs++; a.cfgId = config;
      if (!a.bufPtr) a.bufPtr = host.alloc(a.frames * 2);
      sessions.set(id, a);
      const p = service('media-open', { id, kind: 'audio', deviceId: dev?.deviceId || '', rate: a.rate, frames: a.frames })
        .then(() => { a.stream = id; a.opened = true; return PP_OK; }, (e) => { sessions.delete(id); host.log(`[media] microphone refused: ${e && e.message}`); return failCode(e); });
      if (!cc.func) return p;
      p.then((res) => host.postTask(loopOf(tid), cc.func, [cc.user, res >>> 0]));
      return PP_OK_PENDING;
    },
    GetCurrentConfig(tid, r) { const a = ai(r); if (!a?.cfg) return 0; a.cfg.refs++; return a.cfgId; },
    StartCapture(tid, r) { const a = ai(r); if (!a?.opened) return 0; a.capturing = true; return 1; },
    StopCapture(tid, r) { const a = ai(r); if (!a) return 0; a.capturing = false; a.queue = []; a.queued = 0; a.queueOff = 0; return 1; },
    Close(tid, r) { const a = ai(r); if (a) aiClose(a); },
  });
  impl['PPB_AudioInput(Dev);0.4'] = audioInput(true);       // the version differs in the callback: 0.4 passes the latency, 0.3 does not (Flash 32 asks for 0.3)
  impl['PPB_AudioInput(Dev);0.3'] = audioInput(false);

  // ---- camera ---------------------------------------------------------------------------------------------
  const vc = (r) => host.getRes(r, 'videocapture');
  let pppVideo = 0;
  const ppp = async () => {
    if (!pppVideo) pppVideo = (await host.call(host.getPPP, [host.allocCStr('PPP_VideoCapture(Dev);0.1')])).lo;
    return pppVideo;
  };
  const notify = async (v, slot, args) => { const i = await ppp(); if (i) await host.call(host.u32(i + slot * 4), [v.instance, v.id, ...args]); };
  const vcClose = (v) => {
    if (v.stream != null) { sessions.delete(v.stream); service('media-close', { id: v.stream }).catch(() => {}); v.stream = null; }
    if (v.running) { v.running = false; notify(v, 1, [STATUS_STOPPED]).catch(() => {}); }
    v.opened = false;
  };
  function videoIn(v, m) {
    if (!v.running) return;
    const idx = v.free.pop();
    if (!v.logged) { v.logged = true; host.log(`[media] first camera frame ${m.w}x${m.h}, running=${v.running}, free buffers=${v.free.length + (idx === undefined ? 0 : 1)}`); }
    if (idx === undefined) return;                       // the plugin still holds every buffer: drop the frame
    if (m.w !== v.w || m.h !== v.h) { v.free.push(idx); return; }
    const buf = host.getRes(v.buffers[idx], 'buffer');
    if (!buf) { v.free.push(idx); return; }
    const out = host.bytes(buf.ptr, buf.size);
    rgbaToI420(new Uint8ClampedArray(m.data), v.w, v.h, out);
    notify(v, 3, [idx]).catch(() => {});                 // OnBufferReady(instance, video_capture, buffer)
  }
  impl.PPB_VideoCapture = {
    Create(tid, instance) { const o = { type: 'videocapture', instance, id: 0, stream: null, opened: false, running: false, buffers: [], free: [], w: 0, h: 0, onFree() { vcClose(this); } }; o.id = host.newRes(o); return o.id; },
    IsVideoCapture(tid, r) { return vc(r) ? 1 : 0; },
    EnumerateDevices(tid, r, out, cc) { return vc(r) ? enumerate(tid, r, out, cc, DEV_VIDEO_CAPTURE, 'videoinput', 'Camera') : PP_ERROR_BADRESOURCE; },
    MonitorDeviceChange() { return PP_OK; },
    Open(tid, r, deviceRef, info, count, cc) {
      const v = vc(r); if (!v) return finish(tid, cc, PP_ERROR_BADRESOURCE);
      if (!cc.func && tid === host.mainTid) return PP_ERROR_BLOCKS_MAIN_THREAD;
      const dev = host.getRes(deviceRef, 'deviceref');
      const want = { width: host.u32(info) || 320, height: host.u32(info + 4) || 240, fps: host.u32(info + 8) || 15 };
      const id = ++nextStream; v.stream = id; sessions.set(id, v);
      const p = service('media-open', { id, kind: 'video', deviceId: dev?.deviceId || '', ...want }).then(async (got) => {
        v.w = got.width & ~1; v.h = got.height & ~1;     // I420 needs even dimensions
        const n = Math.max(1, Math.min(MAX_BUFFERS, count || 4)), size = v.w * v.h * 3 / 2;
        v.buffers = []; v.free = [];
        for (let i = 0; i < n; i++) {
          const ptr = host.alloc(size); host.zeroBytes(ptr, size);
          v.buffers.push(host.newRes({ type: 'buffer', size, ptr, onFree() { host.arena.release(ptr); } })); v.free.push(i);
        }
        v.opened = true; v.fps = got.fps;
        const infoPtr = host.alloc(12), bufs = host.alloc(4 * n);
        host.w32(infoPtr, v.w); host.w32(infoPtr + 4, v.h); host.w32(infoPtr + 8, got.fps);
        v.buffers.forEach((b, i) => host.w32(bufs + i * 4, b));
        host.log(`[media] camera opened ${v.w}x${v.h}@${got.fps}, ${n} buffers`);
        await notify(v, 0, [infoPtr, n, bufs]);          // OnDeviceInfo(instance, video_capture, info, buffer_count, buffers)
        return PP_OK;
      }, (e) => { sessions.delete(id); v.stream = null; host.log(`[media] camera refused: ${e && e.message}`); return failCode(e); });
      if (!cc.func) return p;
      p.then((res) => host.postTask(loopOf(tid), cc.func, [cc.user, res >>> 0]));
      return PP_OK_PENDING;
    },
    StartCapture(tid, r) { const v = vc(r); if (!v?.opened) return PP_ERROR_FAILED; v.running = true; notify(v, 1, [STATUS_STARTED]).catch(() => {}); return PP_OK; },
    ReuseBuffer(tid, r, idx) { const v = vc(r); if (!v || idx >= v.buffers.length) return PP_ERROR_BADRESOURCE; if (!v.free.includes(idx)) v.free.push(idx); return PP_OK; },
    StopCapture(tid, r) { const v = vc(r); if (!v) return PP_ERROR_BADRESOURCE; if (v.running) { v.running = false; notify(v, 1, [STATUS_STOPPED]).catch(() => {}); } return PP_OK; },
    Close(tid, r) { const v = vc(r); if (v) vcClose(v); },
  };
  return impl;
}
