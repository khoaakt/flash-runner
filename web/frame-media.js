// Camera and microphone for the player frame: getUserMedia can only run on the frame's main thread, so these services open the devices there and
// stream what they capture to the worker (the 'media' message, handled by ppb-media.js), where Flash's PPB_VideoCapture / PPB_AudioInput read it.
//   media-devices  -> [{ kind: 'audioinput' | 'videoinput', id, label }]            (labels stay empty until the page has been given permission)
//   media-open     {id, kind: 'audio' | 'video', deviceId, width, height, fps, rate, frames} -> {label, width, height, fps} or throws
//   media-close    {id}
// The browser shows its own permission prompt for the extension's origin. The embedding page only delegates camera and microphone to the player
// iframe for the sites the user listed in the `mediaSites` setting (extension/content/content.js); everywhere else getUserMedia is refused by
// permissions policy and Flash sees "no devices".

export function createMedia(player) {
  const open = new Map();           // id -> { stream, stop() }
  const toWorker = (msg, transfer) => player.worker?.postMessage({ type: 'media', ...msg }, transfer || []);

  const stopAll = (id) => { const o = open.get(id); if (o) { open.delete(id); try { o.stop(); } catch { /* already stopped */ } for (const t of o.stream.getTracks()) t.stop(); } };

  async function openVideo({ id, deviceId, width, height, fps }) {
    const video = { width: { ideal: width || 320 }, height: { ideal: height || 240 }, frameRate: { ideal: fps || 15 } };
    if (deviceId) video.deviceId = { exact: deviceId };
    const stream = await navigator.mediaDevices.getUserMedia({ video, audio: false });
    const track = stream.getVideoTracks()[0], st = track.getSettings();
    const w = st.width || width || 320, h = st.height || height || 240, rate = Math.max(1, Math.min(30, st.frameRate || fps || 15));
    const el = document.createElement('video');
    el.muted = true; el.playsInline = true; el.srcObject = stream;
    el.style.cssText = 'position:fixed;left:0;top:0;width:2px;height:2px;opacity:0.01;pointer-events:none';   // a video that is not in the document may not be decoded
    document.body.append(el);
    await el.play();
    const canvas = new OffscreenCanvas(w, h), cx = canvas.getContext('2d', { willReadFrequently: true });
    const timer = setInterval(() => {
      if (el.readyState < 2) return;
      cx.drawImage(el, 0, 0, w, h);
      const data = cx.getImageData(0, 0, w, h).data;
      toWorker({ id, kind: 'video', w, h, data: data.buffer }, [data.buffer]);
    }, 1000 / rate);
    open.set(id, { stream, stop: () => { clearInterval(timer); el.srcObject = null; el.remove(); } });
    return { label: track.label, width: w, height: h, fps: Math.round(rate) };
  }

  async function openAudio({ id, deviceId, rate, frames }) {
    const audio = { echoCancellation: false, noiseSuppression: false, autoGainControl: false, channelCount: 1 };
    if (deviceId) audio.deviceId = { exact: deviceId };
    const stream = await navigator.mediaDevices.getUserMedia({ audio, video: false });
    const ctx = new AudioContext({ sampleRate: rate || 44100 });
    await ctx.resume().catch(() => {});
    const src = ctx.createMediaStreamSource(stream);
    let size = 256; while (size < (frames || 1024) && size < 4096) size *= 2;
    const node = ctx.createScriptProcessor(size, 1, 1);          // the simplest way to get PCM on the main thread in both browsers
    node.onaudioprocess = (e) => { const f = new Float32Array(e.inputBuffer.getChannelData(0)); toWorker({ id, kind: 'audio', samples: f.buffer }, [f.buffer]); };
    const mute = ctx.createGain(); mute.gain.value = 0;           // a processor only runs while it is connected to the destination
    src.connect(node); node.connect(mute); mute.connect(ctx.destination);
    open.set(id, { stream, stop: () => { node.onaudioprocess = null; src.disconnect(); node.disconnect(); mute.disconnect(); ctx.close().catch(() => {}); } });
    return { label: stream.getAudioTracks()[0]?.label || '', rate: ctx.sampleRate };
  }

  return {
    async 'media-devices'() {
      if (!navigator.mediaDevices?.enumerateDevices) return [];
      return (await navigator.mediaDevices.enumerateDevices()).filter((d) => d.kind === 'audioinput' || d.kind === 'videoinput').map((d) => ({ kind: d.kind, id: d.deviceId, label: d.label }));
    },
    async 'media-open'(args) {
      if (!navigator.mediaDevices?.getUserMedia) throw new Error('this browser has no getUserMedia');
      stopAll(args.id);
      return args.kind === 'video' ? openVideo(args) : openAudio(args);
    },
    'media-close'({ id }) { stopAll(id); return true; },
  };
}
