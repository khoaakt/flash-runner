// Read what a test movie painted, from a screenshot (dependency-free PNG decoder for the 8-bit images browsers produce).
// The probe movies in tests/swf-src paint a green 40x40 square at stage (350,250) when their check passed, and some also a 4x4 square at (340,240)
// whose RGB is a number (elapsed milliseconds). The square's size in the screenshot gives the scale, so any window size works.
import { readFileSync } from 'node:fs';
import { inflateSync } from 'node:zlib';

export function decodePng(file) {
  const b = readFileSync(file);
  let p = 8, w = 0, h = 0, colorType = 0; const idat = [];
  while (p < b.length) {
    const len = b.readUInt32BE(p), type = b.toString('latin1', p + 4, p + 8), d = b.subarray(p + 8, p + 8 + len);
    if (type === 'IHDR') { w = d.readUInt32BE(0); h = d.readUInt32BE(4); colorType = d[9]; } else if (type === 'IDAT') idat.push(d);
    p += 12 + len;
  }
  const bpp = colorType === 6 ? 4 : 3, raw = inflateSync(Buffer.concat(idat)), stride = w * bpp, out = Buffer.alloc(h * stride);
  for (let y = 0; y < h; y++) {
    const filter = raw[y * (stride + 1)], row = raw.subarray(y * (stride + 1) + 1, (y + 1) * (stride + 1));
    for (let x = 0; x < stride; x++) {
      const a = x >= bpp ? out[y * stride + x - bpp] : 0, up = y ? out[(y - 1) * stride + x] : 0, c = x >= bpp && y ? out[(y - 1) * stride + x - bpp] : 0;
      let v = row[x];
      if (filter === 1) v += a; else if (filter === 2) v += up; else if (filter === 3) v += (a + up) >> 1;
      else if (filter === 4) { const pa = Math.abs(up - c), pb = Math.abs(a - c), pc = Math.abs(a + up - 2 * c); v += pa <= pb && pa <= pc ? a : pb <= pc ? up : c; }
      out[y * stride + x] = v & 255;
    }
  }
  return { w, h, px: (x, y) => [out[y * stride + x * bpp], out[y * stride + x * bpp + 1], out[y * stride + x * bpp + 2]] };
}

/** { pass, scale, ms }: pass when the green PASS square is there; ms is the number painted in the timing pixel (NaN when there is no PASS square). */
export function probeResult(file) {
  const im = decodePng(file);
  let x0 = 1e9, y0 = 1e9, x1 = -1, y1 = -1;
  for (let y = 0; y < im.h; y++) for (let x = 0; x < im.w; x++) {
    const [r, g, b] = im.px(x, y);
    if (r === 0 && g === 255 && b === 0) { if (x < x0) x0 = x; if (y < y0) y0 = y; if (x > x1) x1 = x; if (y > y1) y1 = y; }
  }
  if (x1 < 0) return { pass: false, scale: NaN, ms: NaN };
  const scale = (x1 - x0 + 1) / 40, [r, g, b] = im.px(Math.round(x0 - 10 * scale + 2 * scale), Math.round(y0 - 10 * scale + 2 * scale));
  return { pass: true, scale, ms: (r << 16) | (g << 8) | b };
}
