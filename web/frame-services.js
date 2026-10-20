// What only the player frame's main thread can do. The emulator runs in a worker, which has no gamepads, file pickers, camera or print dialog,
// so it asks the frame through the 'svc' message (see FlashPlayer.service in loader.js and frameService in worker.js).
// A service is called as fn(args, { active }) where `active` says whether the user has a transient activation (a click or key press a moment ago):
// pickers and print dialogs need one, and a worker round trip is slow enough that the check has to happen here, not there.

export function createFrameServices(player) {
  return {
    /** Round trip check and a look at the activation state. */
    ping() { return { pong: true, active: navigator.userActivation?.isActive ?? null }; },
    /** Open dialog: {accept, multiple} -> [{name, type, bytes}] ([] when the user cancels). Needs a user activation, as every browser file picker does. */
    'pick-files': (args, ctx) => pickFiles(args || {}, ctx),
    /** Save: {name, bytes} -> a download of the bytes (the browser decides where it goes). */
    'save-file': (args) => saveFile(args || {}),
    /** Print: {bytes} (a PDF made by the movie's PrintJob) -> shown to the browser's PDF viewer in a hidden frame and printed; needs a user activation like any print dialog. */
    'print-pdf': (args, ctx) => printPdf(args || {}, ctx),
  };
}

export const MAX_PICKED_BYTES = 64 * 1024 * 1024;

/** PPB_FileChooser accept types ("text/plain", ".txt", "*.txt") as an <input accept> list. */
export function acceptList(accept) {
  return String(accept || '').split(',').map((t) => t.trim().replace(/^\*(?=\.)/, '')).filter(Boolean).map((t) => (t.includes('/') || t.startsWith('.') ? t : '.' + t)).join(',');
}

async function pickFiles({ accept, multiple }, ctx) {
  if (!ctx?.active) throw new Error('a file dialog needs a click or key press first');
  const input = document.createElement('input');
  input.type = 'file'; input.multiple = !!multiple; input.accept = acceptList(accept); input.style.display = 'none';
  document.body.append(input);
  try {
    const files = await new Promise((resolve) => {
      input.addEventListener('change', () => resolve(Array.from(input.files || [])), { once: true });
      input.addEventListener('cancel', () => resolve([]), { once: true });
      input.click();
    });
    const out = [];
    for (const f of files) {
      if (f.size > MAX_PICKED_BYTES) throw new Error(`${f.name} is larger than ${MAX_PICKED_BYTES / 1048576} MiB`);
      out.push({ name: f.name, type: f.type, bytes: new Uint8Array(await f.arrayBuffer()) });
    }
    return out;
  } finally { input.remove(); }
}

function saveFile({ name, bytes }) {
  const a = document.createElement('a');
  a.href = URL.createObjectURL(new Blob([bytes], { type: 'application/octet-stream' }));
  a.download = String(name || 'download').replace(/[\\/:*?"<>|\u0000-\u001f]/g, '_').slice(0, 200) || 'download';
  a.style.display = 'none'; document.body.append(a); a.click();
  setTimeout(() => { a.remove(); URL.revokeObjectURL(a.href); }, 30000);
  return { saved: a.download, size: bytes.length };
}

/** One sample of the connected pads in the layout the worker copies into PP_GamepadsSampleData (at most 4 pads, 16 axes, 32 buttons). */
export function readGamepads() {
  const pads = [];
  for (const g of (navigator.getGamepads?.() || [])) {
    if (!g || !g.connected || pads.length >= 4) continue;
    pads.push({ id: String(g.id), timestamp: g.timestamp, axes: Array.from(g.axes).slice(0, 16), buttons: Array.from(g.buttons, (b) => b.value).slice(0, 32) });
  }
  return pads;
}

/**
 * Poll the Gamepad API and call send(pads) when something changed. Chrome only exposes a pad after a button press, so a
 * pad that appears later is picked up by the 250 ms scan; once a pad exists the poll runs every 16 ms.
 * Returns a function that stops it.
 */
export function watchGamepads(send, { intervalMs = 16 } = {}) {
  if (typeof navigator === 'undefined' || !navigator.getGamepads) return () => {};
  let last = '[]', fast = false;
  const tick = () => {
    const pads = readGamepads();
    const key = JSON.stringify(pads.map((p) => [p.id, p.axes, p.buttons]));
    if (key !== last) { last = key; send(pads); }
    return pads.length > 0;
  };
  let handle = setInterval(tick, 250);
  const watch = setInterval(() => {        // switch between the slow scan and the fast poll as pads come and go
    const have = tick();
    if (have !== fast) { fast = have; clearInterval(handle); handle = setInterval(tick, have ? intervalMs : 250); }
  }, 250);
  tick();
  return () => { clearInterval(handle); clearInterval(watch); };
}

export function pdfInfo(bytes) {
  const head = String.fromCharCode(...bytes.subarray(0, 5));
  const text = new TextDecoder('latin1').decode(bytes);
  return { valid: head === '%PDF-' && /%%EOF\s*$/.test(text), pages: (text.match(/\/Type\s*\/Page(?![A-Za-z])/g) || []).length, size: bytes.length };
}

async function printPdf({ bytes }, ctx) {
  const info = pdfInfo(bytes);
  if (!info.valid) throw new Error('not a PDF');
  if (!ctx?.active) throw new Error('printing needs a click or key press first');
  const url = URL.createObjectURL(new Blob([bytes], { type: 'application/pdf' }));
  const frame = document.createElement('iframe');
  frame.style.cssText = 'position:fixed;left:-10000px;top:0;width:816px;height:1056px;border:0';
  frame.src = url; document.body.append(frame);
  await new Promise((resolve) => { frame.addEventListener('load', resolve, { once: true }); setTimeout(resolve, 3000); });
  let via = 'frame';
  try { frame.contentWindow.focus(); frame.contentWindow.print(); } catch { window.open(url, '_blank'); via = 'tab'; }   // a viewer that cannot be printed from a frame: show it in a tab
  setTimeout(() => { frame.remove(); URL.revokeObjectURL(url); }, 120000);
  return { ...info, via };
}
