// How the test drivers see the PDF the player frame is asked to print. ARM_* run inside the frame (Chrome: Runtime.evaluate, Firefox: executeAsync)
// and keep the PDF; REPORT_* fetch it as base64 and `describePdf` inflates its first image in Node.
//   Chrome:  URL.createObjectURL is wrapped (the page's own functions are reachable from a CDP evaluation).
//   Firefox: the driver's sandbox cannot patch page functions, so a MutationObserver sees the hidden <iframe src="blob:..."> the frame adds, fetches
//            it and keeps the bytes as a base64 attribute on <html>.
import { inflateSync } from 'node:zlib';

export const ARM_CHROME = `(() => { if (!window.__pdfSpy) { window.__pdfSpy = { blobs: [], b64: '' }; const o = URL.createObjectURL.bind(URL); URL.createObjectURL = (b) => { if (b && b.type === 'application/pdf') { window.__pdfSpy.blobs.push(b); b.arrayBuffer().then((a) => { let s = ''; const u = new Uint8Array(a); for (let i = 0; i < u.length; i++) s += String.fromCharCode(u[i]); window.__pdfSpy.b64 = btoa(s); }); } return o(b); }; } return 'armed'; })()`;
export const REPORT_CHROME = `({ prints: window.__pdfSpy ? window.__pdfSpy.blobs.length : 0, b64: window.__pdfSpy ? window.__pdfSpy.b64 : '' })`;
export const ARM_FIREFOX = `(() => { new MutationObserver((muts) => { for (const m of muts) for (const n of m.addedNodes) if (n.localName === 'iframe' && String(n.src).startsWith('blob:')) {
  fetch(n.src).then((r) => r.arrayBuffer()).then((a) => { let s = ''; const u = new Uint8Array(a); for (let i = 0; i < u.length; i++) s += String.fromCharCode(u[i]); document.documentElement.setAttribute('data-pdf', btoa(s)); document.documentElement.setAttribute('data-pdf-count', String(Number(document.documentElement.getAttribute('data-pdf-count') || 0) + 1)); });
} }).observe(document.body, { childList: true }); return 'armed'; })()`;
export const REPORT_FIREFOX = `({ prints: Number(document.documentElement.getAttribute('data-pdf-count') || 0), b64: document.documentElement.getAttribute('data-pdf') || '' })`;

/** {prints, b64} -> what the movie put on the page (null fields when the PDF is not shaped as Flash's is). */
export function describePdf({ prints, b64 }) {
  if (!b64) return { prints };
  const bytes = Buffer.from(b64, 'base64'), text = bytes.toString('latin1');
  const media = /MediaBox \[(\d+) (\d+) (\d+) (\d+)\]/.exec(text), img = /\/Height (\d+)[\s\S]*?\/Width (\d+)[\s\S]*?stream\n/.exec(text);
  let pixel = null;
  if (img) {
    const start = img.index + img[0].length, end = text.indexOf('\nendstream', start);
    try { pixel = Array.from(inflateSync(bytes.subarray(start, end)).subarray(0, 3)); } catch { pixel = null; }
  }
  return { prints, header: text.slice(0, 8), mediaBox: media ? media.slice(1).map(Number) : null, image: img ? [Number(img[2]), Number(img[1])] : null, firstPixel: pixel };
}

/** What the PrintProbe movie must have put on its PDF: one print, a Letter page, its red 100x100 image. */
export const expectedPrint = (r) => !!r && r.prints === 1 && r.header === '%PDF-1.4' && JSON.stringify(r.mediaBox) === '[0,0,612,792]' && JSON.stringify(r.image) === '[100,100]' && JSON.stringify(r.firstPixel) === '[255,0,0]';
