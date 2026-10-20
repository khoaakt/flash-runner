// Generated media for the camera and microphone tests (nothing is downloaded or committed): a 320x240 Y4M clip whose left half is red and right half
// blue with a white bar that moves a step every frame, and a 44.1 kHz mono WAV with a loud 440 Hz tone. Chrome plays them as its fake camera and
// microphone through --use-file-for-fake-video-capture / --use-file-for-fake-audio-capture.
import { writeFileSync } from 'node:fs';

export const CLIP = { width: 320, height: 240, fps: 15, frames: 15 };

export function writeFakeCamera(path) {
  const { width: w, height: h, fps, frames } = CLIP;
  const parts = [Buffer.from(`YUV4MPEG2 W${w} H${h} F${fps}:1 Ip A1:1 C420jpeg\n`)];
  for (let f = 0; f < frames; f++) {
    const y = Buffer.alloc(w * h), u = Buffer.alloc((w / 2) * (h / 2)), v = Buffer.alloc((w / 2) * (h / 2));
    for (let r = 0; r < h; r++) for (let c = 0; c < w; c++) y[r * w + c] = c < w / 2 ? 81 : 41;                 // red | blue luma
    for (let r = 0; r < h / 2; r++) for (let c = 0; c < w / 2; c++) { u[r * (w / 2) + c] = c < w / 4 ? 90 : 240; v[r * (w / 2) + c] = c < w / 4 ? 240 : 110; }
    const bar = (f * 16) % (h - 16);
    for (let r = bar; r < bar + 16; r++) for (let c = 0; c < w; c++) y[r * w + c] = 235;                          // the moving bar (luma only: it stays light)
    parts.push(Buffer.from('FRAME\n'), y, u, v);
  }
  writeFileSync(path, Buffer.concat(parts));
}

export function writeFakeMicrophone(path, seconds = 2, rate = 44100) {
  const n = seconds * rate, data = Buffer.alloc(n * 2);
  for (let i = 0; i < n; i++) data.writeInt16LE(Math.round(Math.sin((2 * Math.PI * 440 * i) / rate) * 20000), i * 2);
  const head = Buffer.alloc(44);
  head.write('RIFF', 0); head.writeUInt32LE(36 + data.length, 4); head.write('WAVEfmt ', 8); head.writeUInt32LE(16, 16); head.writeUInt16LE(1, 20); head.writeUInt16LE(1, 22);
  head.writeUInt32LE(rate, 24); head.writeUInt32LE(rate * 2, 28); head.writeUInt16LE(2, 32); head.writeUInt16LE(16, 34); head.write('data', 36); head.writeUInt32LE(data.length, 40);
  writeFileSync(path, Buffer.concat([head, data]));
}
