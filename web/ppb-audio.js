// PPB_AudioConfig / PPB_Audio. The plugin's callback runs on a dedicated guest thread (message loop 2,
// created by pepper-host); we pull fixed-size buffers whenever the output sink runs low and push the
// PCM into the sink (an AudioWorklet ring in the browser, an array in tests).
import { PP_OK, PP_ERROR_BADRESOURCE, PP_ERROR_BADARGUMENT, PP_ERROR_FAILED } from './pp-consts.js';

export const AUDIO_LOOP = 2;

export function buildAudio(host) {
  const impl = {};
  const rate = () => host.opts.sampleRate || 48000;

  const recommendFrames = (requested) => {
    const n = Math.max(64, Math.min(32768, requested | 0));
    return Math.min(2 ** Math.ceil(Math.log2(n)), 32768);       // powers of two keep hardware happy
  };
  impl.PPB_AudioConfig = {
    RecommendSampleRate() { return rate(); },
    RecommendSampleFrameCount(tid, instance, sampleRate, requested) { return recommendFrames(requested); },
    CreateStereo16Bit(tid, instance, sampleRate, frames) {
      if (frames < 64 || frames > 32768) return 0;
      return host.newRes({ type: 'audioconfig', rate: sampleRate, frames });
    },
    IsAudioConfig(tid, r) { return host.getRes(r, 'audioconfig') ? 1 : 0; },
    GetSampleRate(tid, r) { return host.getRes(r, 'audioconfig')?.rate ?? 0; },
    GetSampleFrameCount(tid, r) { return host.getRes(r, 'audioconfig')?.frames ?? 0; },
  };

  // PPB_AudioConfig;1.0 has no `instance` parameter in RecommendSampleFrameCount and no RecommendSampleRate.
  impl['PPB_AudioConfig;1.0'] = { ...impl.PPB_AudioConfig, RecommendSampleFrameCount(tid, sampleRate, requested) { return recommendFrames(requested); } };

  const mk = (legacy) => ({
    Create(tid, instance, config, cb, user) {
      const cfg = host.getRes(config, 'audioconfig');
      if (!cfg || !cb) return 0;
      const bytes = cfg.frames * 4;
      const bufPtr = host.alloc(bytes);
      cfg.refs++;                                              // the audio object keeps its configuration alive
      const a = { type: 'audio', instance, cfg, cfgId: config, cb, user, legacy, bufPtr, bytes, playing: false, busy: false, freed: false, onIdle: null,
        onFree() { a.freed = true; a.playing = false; host.audioDriver.remove(a); host.releaseRes(a.cfgId); if (!a.busy) host.arena.release(bufPtr); } };
      return host.newRes(a);
    },
    IsAudio(tid, r) { return host.getRes(r, 'audio') ? 1 : 0; },
    GetCurrentConfig(tid, r) { const a = host.getRes(r, 'audio'); return a ? findCfgId(a) : 0; },
    StartPlayback(tid, r) { const a = host.getRes(r, 'audio'); if (!a) return 0; a.playing = true; host.audioDriver.add(a); return 1; },
    StopPlayback(tid, r) {
      const a = host.getRes(r, 'audio'); if (!a) return 0;
      a.playing = false; host.audioDriver.remove(a);
      if (a.busy && tid !== host.mainTid) return new Promise((res) => { a.onIdle = () => res(1); });   // wait for the in-flight callback
      return 1;
    },
  });
  function findCfgId(a) { if (!host.res.has(a.cfgId)) return 0; a.cfg.refs++; return a.cfgId; }
  impl['PPB_Audio;1.0'] = mk(true);
  impl['PPB_Audio;1.1'] = mk(false);

  // PPB_AudioOutput(Dev);0.1: what Flash 32 uses instead of PPB_Audio. Same pull model; the device is opened asynchronously.
  const u32le = (n) => new Uint8Array([n & 255, (n >> 8) & 255, (n >> 16) & 255, (n >> 24) & 255]);
  impl['PPB_AudioOutput(Dev);0.1'] = {
    Create(tid, instance) {
      const a = { type: 'audiooutput', instance, cfg: null, cfgId: 0, cb: 0, user: 0, legacy: false, bufPtr: 0, bytes: 0, playing: false, busy: false, freed: false, onIdle: null,
        onFree() { a.freed = true; a.playing = false; host.audioDriver.remove(a); if (a.cfgId) host.releaseRes(a.cfgId); if (a.bufPtr && !a.busy) host.arena.release(a.bufPtr); } };
      return host.newRes(a);
    },
    IsAudioOutput(tid, r) { return host.getRes(r, 'audiooutput') ? 1 : 0; },
    EnumerateDevices(tid, r, out, cc) {
      if (!host.getRes(r, 'audiooutput')) return PP_ERROR_BADRESOURCE;
      const ref = host.newRes({ type: 'deviceref', devType: 3, name: 'Default' });
      return host.arrayThenCallback(tid, out, 4, [u32le(ref)], cc);
    },
    MonitorDeviceChange() { return PP_OK; },
    Open(tid, r, device, config, cb, user, cc) {
      const a = host.getRes(r, 'audiooutput'), cfg = host.getRes(config, 'audioconfig');
      if (!a || !cfg || !cb) return host.complete(tid, cc, a ? PP_ERROR_BADARGUMENT : PP_ERROR_BADRESOURCE);
      if (a.cfg) return host.complete(tid, cc, PP_ERROR_FAILED);            // already open
      cfg.refs++; a.cfg = cfg; a.cfgId = config; a.cb = cb; a.user = user;
      a.bytes = cfg.frames * 4; a.bufPtr = host.alloc(a.bytes);
      return host.complete(tid, cc, PP_OK);
    },
    GetCurrentConfig(tid, r) { const a = host.getRes(r, 'audiooutput'); if (!a || !a.cfg) return 0; a.cfg.refs++; return a.cfgId; },
    StartPlayback(tid, r) { const a = host.getRes(r, 'audiooutput'); if (!a || !a.cfg) return 0; a.playing = true; host.audioDriver.add(a); return 1; },
    StopPlayback(tid, r) {
      const a = host.getRes(r, 'audiooutput'); if (!a) return 0;
      a.playing = false; host.audioDriver.remove(a);
      if (a.busy && tid !== host.mainTid) return new Promise((res) => { a.onIdle = () => res(1); });
      return 1;
    },
    Close(tid, r) { const a = host.getRes(r, 'audiooutput'); if (a) { a.playing = false; host.audioDriver.remove(a); } },
  };

  // ---- pull driver ---------------------------------------------------------------------
  const driver = host.audioDriver = {
    set: new Set(), timer: null, produced: 0, t0: 0,
    add(a) { this.set.add(a); this.kick(); },
    remove(a) { this.set.delete(a); },
    kick() {
      if (this.timer) return;
      this.t0 = performance.now(); this.produced = 0;
      this.timer = setInterval(() => this.tick(), host.opts.audioTickMs || 5);
    },
    tick() {
      if (!this.set.size) { clearInterval(this.timer); this.timer = null; return; }
      const sink = host.opts.audioSink; if (!sink) return;
      for (const a of this.set) {
        if (a.busy) continue;
        const target = sink.targetFrames ? sink.targetFrames() : Math.round(rate() * 0.12);
        if (sink.bufferedFrames() >= target) continue;
        a.busy = true;
        const latency = sink.bufferedFrames() / (sink.rate || a.cfg.rate);
        const lb = new DataView(new ArrayBuffer(8)); lb.setFloat64(0, latency, true);
        const args = a.legacy ? [a.bufPtr, a.bytes, a.user] : [a.bufPtr, a.bytes, lb.getUint32(0, true), lb.getUint32(4, true), a.user];
        let ran = false;
        const done = () => {
          if (ran) return; ran = true;
          a.busy = false;
          if (a.onIdle) { const f = a.onIdle; a.onIdle = null; f(); }
          if (a.freed) { host.arena.release(a.bufPtr); return; }
          if (!a.playing) return;
          const s16 = new Int16Array(host.ro(a.bufPtr, a.bytes).slice().buffer);
          const f = new Float32Array(s16.length);
          for (let i = 0; i < s16.length; i++) f[i] = s16[i] / 32768;
          sink.write(f, a.cfg.rate);
          this.tick();                                   // keep producing until the sink is full (small buffers)
        };
        host.postTask(AUDIO_LOOP, a.cb, args, 0, false, { post: { addr: a.bufPtr, len: a.bytes }, sync: done }).then(done);   // `sync` runs inside the completion (buffer already shipped); then() is the fallback
      }
    },
  };
  return impl;
}
