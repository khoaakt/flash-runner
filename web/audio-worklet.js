// AudioWorkletProcessor that drains an interleaved-stereo float ring buffer living in a SharedArrayBuffer.
// Layout: Int32Array header [writeFrame, readFrame, capacityFrames, underruns], Float32Array data.
class FlashAudio extends AudioWorkletProcessor {
  constructor(opts) {
    super();
    const { sab, queueMode } = opts.processorOptions;
    this.queueMode = queueMode;
    if (sab) {
      this.h = new Int32Array(sab, 0, 4);
      this.d = new Float32Array(sab, 16);
      this.cap = Atomics.load(this.h, 2);
    } else {
      this.q = []; this.qoff = 0; this.queued = 0;
      this.port.onmessage = (e) => { this.q.push(e.data); this.queued += e.data.length / 2; };
    }
  }
  process(inputs, outputs) {
    const out = outputs[0], L = out[0], R = out[1] || out[0], n = L.length;
    if (this.h) {
      const w = Atomics.load(this.h, 0), r = Atomics.load(this.h, 1);
      let avail = w - r;
      if (avail < 0) avail += this.cap;
      if (avail >= n) {
        for (let i = 0, p = r; i < n; i++, p++) { if (p >= this.cap) p = 0; L[i] = this.d[p * 2]; R[i] = this.d[p * 2 + 1]; }
        Atomics.store(this.h, 1, (r + n) % this.cap);
      } else { Atomics.add(this.h, 3, 1); }
    } else {
      let i = 0;
      while (i < n && this.q.length) {
        const c = this.q[0];
        while (i < n && this.qoff < c.length) { L[i] = c[this.qoff]; R[i] = c[this.qoff + 1]; this.qoff += 2; i++; }
        if (this.qoff >= c.length) { this.q.shift(); this.qoff = 0; }
      }
      this.queued -= i;
      this.port.postMessage(this.queued);
    }
    return true;
  }
}
registerProcessor('flash-audio', FlashAudio);
