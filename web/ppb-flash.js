// Flash-specific and "Dev"/"Private" PPB interfaces Pepper Flash probes for at startup.
// Anything not implemented here falls through to the generic "unimplemented, returns 0" path in pepper.js.
import { PP_OK, PP_OK_COMPLETIONPENDING, PP_ERROR_FAILED, PP_ERROR_BADRESOURCE, PP_ERROR_BADARGUMENT, PP_ERROR_NOTSUPPORTED, PP_ERROR_ABORTED, PP_ERROR_FILENOTFOUND, PP_ERROR_USERCANCEL, VT } from './pp-consts.js';

const LOCAL_ROOT = '/home/flash/.flash/ModuleLocal';
const O_RDONLY = 0, O_WRONLY = 1, O_RDWR = 2, O_CREAT = 0x40, O_EXCL = 0x80, O_TRUNC = 0x200;

// ---- local timezone offset (PPB_Flash::GetLocalTimeZoneOffset) ----
// AVM2 Date spams this call (60k per 3 s window in a live game log). V8 keeps its own cache of the DST intervals, so asking it directly is
// ~0.1 us and exact for every zone and timestamp. (An interval cache of our own used to sit here; its lookup never matched, so every call ran
// the ~40 Date probes that were meant to fill it, and a working version must prove its interval edges for every zone.)
const tzOff = (t) => -new Date(t * 1000).getTimezoneOffset() * 60;

export function buildFlash(host) {
  const impl = {};
  const dec = new TextDecoder();

  impl.PPB_Flash = {
    SetInstanceAlwaysOnTop() {},
    DrawGlyphs() { return 0; },                               // TODO: device-font glyph drawing
    GetProxyForURL() { return host.mkVar('DIRECT'); },
    Navigate(tid, req, target, user) {
      const q = host.getRes(req, 'urlrequest'); if (!q) return PP_ERROR_BADRESOURCE;
      host.opts.onNavigate?.(new URL(q.url, host.opts.baseUrl || 'http://localhost/').href, host.cstr(target), !!user);
      return PP_OK;
    },
    GetLocalTimeZoneOffset(tid, instance, t) { return tzOff(t); },   // see tzOff
    GetCommandLineArgs() { return host.mkVar(''); },
    PreloadFontWin() {},
    IsRectTopmost() { return 1; },
    UpdateActivity() {},
    GetSetting(tid, instance, setting) {
      switch (setting) {            // PP_FlashSetting is 1-based (ppb_flash.h)
        case 1: return host.mkVar(!!host.opts.gpu3d);              // PP_FLASHSETTING_3DENABLED
        case 2: return host.mkVar(false);                          // INCOGNITO
        case 3: return host.mkVar(!!host.opts.gpu3d);              // STAGE3DENABLED
        case 4: return host.mkVar(host.opts.language || (typeof navigator !== 'undefined' ? navigator.language : 'en-US'));  // LANGUAGE
        case 5: return host.mkVar(host.opts.numCores || 1);        // NUMCORES: guest threads share one core
        case 6: return host.mkVar(1);                              // LSORESTRICTIONS: PP_FLASHLSORESTRICTIONS_NONE
        case 7: return host.mkVar(!!host.opts.gpu3d);              // STAGE3DBASELINEENABLED
        default: return host.mkVar(undefined);
      }
    },
    SetCrashData() { return 1; },
    EnumerateVideoCaptureDevices() { return PP_ERROR_FAILED; },
  };

  // ---- Flash_File_ModuleLocal: Shared Objects / settings storage as real guest files ------
  const safe = (p) => {
    const parts = host.cstr(p).split('/').filter((x) => x && x !== '.' && x !== '..');
    return LOCAL_ROOT + '/' + parts.join('/');
  };
  const emu = () => host.emu;
  let dirty = null;
  const touch = () => { if (host.opts.persist && !dirty) dirty = setTimeout(() => { dirty = null; host.persistStorage(); }, 1500); };
  host.touchStorage = touch;
  impl['PPB_Flash_File_ModuleLocal'] = {
    CreateThreadAdapterForInstance() { return 1; },
    ClearThreadAdapterForInstance() {},
    OpenFile(tid, instance, path, mode, filePtr) {
      let fl = (mode & 3) === 3 ? O_RDWR : (mode & 2) ? O_WRONLY : O_RDONLY;
      if (mode & 4) fl |= O_CREAT;
      if (mode & 8) fl |= O_TRUNC;
      if (mode & 16) fl |= O_EXCL;
      const full = safe(path);
      if (fl & O_CREAT) emu().mkdirp(full.slice(0, full.lastIndexOf('/')));
      const fd = emu().openFd(full, fl);
      if (fd < 0) { host.wi32(filePtr, -1); return fd === -2 ? PP_ERROR_FILENOTFOUND : PP_ERROR_FAILED; }
      host.wi32(filePtr, fd);
      if (fl & (O_WRONLY | O_RDWR)) touch();
      return PP_OK;
    },
    RenameFile(tid, instance, from, to) { const rc = emu().rename(safe(from), safe(to)); touch(); return rc === 0 ? PP_OK : PP_ERROR_FAILED; },
    DeleteFileOrDir(tid, instance, path, recursive) { const rc = emu().unlinkPath(safe(path), !!recursive); touch(); return rc === 0 ? PP_OK : PP_ERROR_FILENOTFOUND; },
    CreateDir(tid, instance, path) { const rc = emu().mkdirp(safe(path)); touch(); return rc === 0 ? PP_OK : PP_ERROR_FAILED; },
    QueryFile(tid, instance, path, infoPtr) {
      const st = emu().stat(safe(path)); if (!st) return PP_ERROR_FILENOTFOUND;
      writeFileInfo(infoPtr, st); return PP_OK;
    },
    GetDirContents(tid, instance, path, outPtr) {
      const list = emu().readdir(safe(path)); if (!list) return PP_ERROR_FILENOTFOUND;
      const entries = host.alloc(Math.max(8, list.length * 8));
      list.forEach((e, i) => { host.w32(entries + i * 8, host.allocCStr(e.name)); host.wi32(entries + i * 8 + 4, e.dir ? 1 : 0); });
      const hdr = host.alloc(8); host.wi32(hdr, list.length); host.w32(hdr + 4, entries);
      host.w32(outPtr, hdr); return PP_OK;
    },
    FreeDirContents(tid, instance, contents) {
      if (!contents) return;
      const n = host.i32(contents), ents = host.u32(contents + 4);
      for (let i = 0; i < n; i++) host.arena.release(host.u32(ents + i * 8));
      host.arena.release(ents); host.arena.release(contents);
    },
    CreateTemporaryFile(tid, instance, filePtr) {
      const name = `${LOCAL_ROOT}/tmp/t${host.nextTmp = (host.nextTmp || 0) + 1}`;
      emu().mkdirp(LOCAL_ROOT + '/tmp');
      const fd = emu().openFd(name, O_RDWR | O_CREAT | O_TRUNC);
      if (fd < 0) return PP_ERROR_FAILED;
      emu().unlinkPath(name);                      // anonymous: the open descriptor keeps the data alive
      host.wi32(filePtr, fd); return PP_OK;
    },
  };
  function writeFileInfo(p, st) {
    host.w32(p, st.size >>> 0); host.w32(p + 4, Math.floor(st.size / 2 ** 32));
    host.wi32(p + 8, (st.mode & 0o170000) === 0o040000 ? 1 : 0);   // PP_FILETYPE_DIRECTORY / REGULAR
    host.wi32(p + 12, 0);
    for (const o of [16, 24, 32]) host.wf64(p + o, st.mtime);
  }
  impl['PPB_Flash_File_FileRef'] = {
    OpenFile(tid, ref, mode, filePtr) { return PP_ERROR_NOTSUPPORTED; },
    QueryFile(tid, ref, info) { return PP_ERROR_NOTSUPPORTED; },
  };

  impl.PPB_FlashFullscreen = {
    IsFullscreen(tid, instance) { return host.instances.get(instance)?.fullscreen ? 1 : 0; },
    SetFullscreen(tid, instance, on) { host.opts.onFullscreen?.(instance, !!on); return 1; },
    GetScreenSize(tid, instance, p) { const s = host.opts.screenSize || { w: 1920, h: 1080 }; host.wi32(p, s.w); host.wi32(p + 4, s.h); return 1; },
  };

  impl.PPB_Flash_Clipboard = {
    RegisterCustomFormat(tid, instance, namePtr) { host.customFormats = host.customFormats || []; host.customFormats.push(host.cstr(namePtr)); return 3 + host.customFormats.length; },
    IsFormatAvailable(tid, instance, type, format) { return format === 1 && host.clipboardText ? 1 : 0; },
    ReadData(tid, instance, type, format) { return format === 1 && host.clipboardText ? host.mkVar(host.clipboardText) : host.mkVar(undefined); },
    WriteData(tid, instance, type, count, formatsPtr, itemsPtr) {
      for (let i = 0; i < count; i++) {
        if (host.u32(formatsPtr + i * 4) === 1) { const v = host.readVar(itemsPtr + i * 16); host.clipboardText = host.varValue(v); host.opts.onClipboardWrite?.(host.clipboardText); }
      }
      return PP_OK;
    },
    GetSequenceNumber(tid, instance, type, p) { host.w32(p, 0); host.w32(p + 4, 0); return 1; },
  };

  // ---- Flash context menu (right click): the page shows an HTML menu and reports the chosen item id -----------------
  function readMenu(p, depth = 0) {                          // PP_Flash_Menu { uint32 count; PP_Flash_MenuItem* items }
    if (!p || depth > 6) return [];
    const count = Math.min(host.u32(p), 256), items = host.u32(p + 4), out = [];
    for (let i = 0; i < count; i++) {
      const q = items + i * 24;                              // { type, char* name, id, enabled, checked, submenu* }
      const namePtr = host.u32(q + 4);
      out.push({ type: host.i32(q), name: namePtr ? host.cstr(namePtr) : '', id: host.i32(q + 8), enabled: !!host.i32(q + 12), checked: !!host.i32(q + 16), submenu: host.u32(q + 20) ? readMenu(host.u32(q + 20), depth + 1) : null });
    }
    return out;
  }
  const pendingMenus = host.pendingMenus = new Map();
  let menuSeq = 0;
  host.menuResult = (id, selected) => { const f = pendingMenus.get(id); if (f) { pendingMenus.delete(id); f(selected); } };
  impl.PPB_Flash_Menu = {
    Create(tid, instance, dataPtr) { return host.newRes({ type: 'flashmenu', instance, items: readMenu(dataPtr) }); },
    IsFlashMenu(tid, r) { return host.getRes(r, 'flashmenu') ? 1 : 0; },
    Show(tid, menu, loc, selectedPtr, cc) {
      const m = host.getRes(menu, 'flashmenu'); if (!m) return PP_ERROR_BADRESOURCE;
      if (!host.opts.onMenu) { host.wi32(selectedPtr, -1); return host.complete(tid, cc, PP_ERROR_USERCANCEL); }
      const id = ++menuSeq, x = host.i32(loc), y = host.i32(loc + 4);
      return host.finishAsync(tid, cc, host.threadLoop.get(tid) || 1, (done) => {
        pendingMenus.set(id, (sel) => { if (sel == null) { host.wi32(selectedPtr, -1); done(PP_ERROR_USERCANCEL); } else { host.wi32(selectedPtr, sel); done(PP_OK); } });
        host.opts.onMenu(m.instance, id, m.items, x, y);
      });
    },
  };

  impl.PPB_Flash_FontFile = {
    Create() { return 0; },
    IsFlashFontFile() { return 0; },
    GetFontTable() { return 0; },
    IsSupportedForWindows() { return 0; },
  };

  // ---- PPB_BrowserFont_Trusted: device-font text through the browser's own font stack (canvas 2D) ----------------
  // Flash draws every non-embedded TextField with these calls, so without them device text is invisible.
  const FAMILY_CSS = ['sans-serif', 'serif', 'sans-serif', 'monospace'];      // DEFAULT, SERIF, SANSSERIF, MONOSPACE
  let measureCtx = null, drawCanvas = null, drawCtx = null;
  const hasCanvas = typeof OffscreenCanvas !== 'undefined';
  const fontCss = (f) => {
    const face = f.face ? `"${f.face.replace(/"/g, '')}", ` : '';
    return `${f.italic ? 'italic ' : ''}${f.weight} ${Math.max(1, f.size)}px ${face}${FAMILY_CSS[f.family] || 'sans-serif'}`;
  };
  const applyFont = (ctx, f) => {
    ctx.font = fontCss(f);
    try { ctx.letterSpacing = (f.letterSpacing | 0) + 'px'; ctx.wordSpacing = (f.wordSpacing | 0) + 'px'; } catch { /* older engines */ }
  };
  const mctx = () => (measureCtx ||= new OffscreenCanvas(1, 1).getContext('2d', { willReadFrequently: true }));
  function metricsOf(f) {
    if (!hasCanvas) { const h = Math.round(f.size * 1.2); return { height: h, ascent: Math.round(f.size * 0.9), descent: h - Math.round(f.size * 0.9), lineSpacing: h + 1, xHeight: Math.round(f.size * 0.5) }; }
    const c = mctx(); applyFont(c, f);
    const m = c.measureText('Hxgjy');
    const asc = Math.ceil(m.fontBoundingBoxAscent ?? m.actualBoundingBoxAscent ?? f.size * 0.9);
    const desc = Math.ceil(m.fontBoundingBoxDescent ?? m.actualBoundingBoxDescent ?? f.size * 0.25);
    const xh = Math.round(c.measureText('x').actualBoundingBoxAscent || f.size * 0.5);
    return { height: asc + desc, ascent: asc, descent: desc, lineSpacing: Math.ceil((asc + desc) * 1.05), xHeight: xh };
  }
  const widthOf = (f, text) => {
    if (!hasCanvas) return Math.round(text.length * f.size * 0.55);
    const c = mctx(); applyFont(c, f); return c.measureText(text).width;
  };
  const readRun = (p) => host.varValue(host.readVar(p)) ?? '';          // PP_BrowserFont_Trusted_TextRun starts with the PP_Var text
  const font = (r) => host.getRes(r, 'font');
  impl.PPB_BrowserFont_Trusted = {
    GetFontFamilies() { return host.mkVar(['Arial', 'Helvetica', 'Verdana', 'Tahoma', 'Trebuchet MS', 'Times New Roman', 'Georgia', 'Courier New', 'Monaco', 'Menlo', 'sans-serif', 'serif', 'monospace'].join('\0') + '\0'); },
    Create(tid, instance, d) {
      const f = { type: 'font', face: host.varValue(host.readVar(d)) || '', family: host.i32(d + 16), size: host.u32(d + 20) || 12, weight: [100, 200, 300, 400, 500, 600, 700, 800, 900][host.i32(d + 24)] ?? 400,
        italic: !!host.i32(d + 28), smallCaps: !!host.i32(d + 32), letterSpacing: host.i32(d + 36), wordSpacing: host.i32(d + 40) };
      if (typeof f.face !== 'string') f.face = '';
      return host.newRes(f);
    },
    IsFont(tid, r) { return font(r) ? 1 : 0; },
    Describe(tid, r, d, m) {
      const f = font(r); if (!f) return 0;
      host.writeVar(d, host.mkVar(f.face));
      host.wi32(d + 16, f.family); host.w32(d + 20, f.size); host.wi32(d + 24, [100, 200, 300, 400, 500, 600, 700, 800, 900].indexOf(f.weight));
      host.wi32(d + 28, f.italic ? 1 : 0); host.wi32(d + 32, f.smallCaps ? 1 : 0); host.wi32(d + 36, f.letterSpacing); host.wi32(d + 40, f.wordSpacing); host.wi32(d + 44, 0);
      if (m) { const x = metricsOf(f); host.wi32(m, x.height); host.wi32(m + 4, x.ascent); host.wi32(m + 8, x.descent); host.wi32(m + 12, x.lineSpacing); host.wi32(m + 16, x.xHeight); }
      return 1;
    },
    MeasureText(tid, r, run) { const f = font(r); return f ? Math.ceil(widthOf(f, readRun(run))) : -1; },
    CharacterOffsetForPixel(tid, r, run, px) {
      const f = font(r); if (!f) return 0; const t = readRun(run);
      for (let i = 0; i < t.length; i++) { const w = widthOf(f, t.slice(0, i + 1)), w0 = widthOf(f, t.slice(0, i)); if (px < (w + w0) / 2) return i; }
      return t.length;
    },
    PixelOffsetForCharacter(tid, r, run, off) { const f = font(r); return f ? Math.round(widthOf(f, readRun(run).slice(0, off))) : 0; },
    DrawTextAt(tid, r, image, run, pos, color, clip, opaque) {
      const f = font(r), im = host.getRes(image, 'imagedata'); if (!f || !im || !hasCanvas) return f && im ? 1 : 0;
      const text = readRun(run); if (!text) return 1;
      const m = metricsOf(f), w = Math.max(1, Math.ceil(widthOf(f, text)) + 4), h = m.height + 4;
      if (!drawCanvas || drawCanvas.width < w || drawCanvas.height < h) { drawCanvas = new OffscreenCanvas(Math.max(w, 256), Math.max(h, 64)); drawCtx = drawCanvas.getContext('2d', { willReadFrequently: true }); }
      const c = drawCtx; c.clearRect(0, 0, drawCanvas.width, drawCanvas.height);
      applyFont(c, f); c.textBaseline = 'alphabetic';
      const A = (color >>> 24) & 255 || 255, R = (color >>> 16) & 255, G = (color >>> 8) & 255, B = color & 255;
      c.fillStyle = `rgb(${R},${G},${B})`; c.fillText(text, 2, m.ascent + 2);
      const src = c.getImageData(0, 0, w, h).data;                    // straight RGBA, coverage in alpha
      let x0 = host.i32(pos) - 2, y0 = host.i32(pos + 4) - m.ascent - 2;   // `pos` is the baseline origin
      let cx0 = 0, cy0 = 0, cx1 = im.w, cy1 = im.h;
      if (clip) { cx0 = Math.max(0, host.i32(clip)); cy0 = Math.max(0, host.i32(clip + 4)); cx1 = Math.min(im.w, cx0 + host.i32(clip + 8)); cy1 = Math.min(im.h, cy0 + host.i32(clip + 12)); }
      const xa = Math.max(cx0, x0), xb = Math.min(cx1, x0 + w), ya = Math.max(cy0, y0), yb = Math.min(cy1, y0 + h);   // destination rect, clipped
      if (xa >= xb || ya >= yb) return 1;
      const bw = xb - xa, bh = yb - ya, cov = new Uint8Array(bw * bh);
      for (let y = 0; y < bh; y++) for (let x = 0; x < bw; x++) cov[y * bw + x] = src[((ya - y0 + y) * w + (xa - x0 + x)) * 4 + 3];
      host.blendCoverage(im.ptr + (ya * im.w + xa) * 4, im.w * 4, bw, bh, R, G, B, A, cov);
      return 1;
    },
  };

  // ---- Dev interfaces ---------------------------------------------------------------------
  impl.PPB_Memory = {
    MemAlloc(tid, n) { return n ? host.alloc(n) : 0; },
    MemFree(tid, p) { if (p) host.arena.release(p); },
  };
  impl.PPB_Crypto = {
    GetRandomBytes(tid, buf, n) { for (let o = 0; o < n; o += 65536) host.putBytes(buf + o, crypto.getRandomValues(new Uint8Array(Math.min(65536, n - o)))); },
  };
  impl.PPB_Buffer = {
    Create(tid, instance, n) { if (n <= 0 || n > 1 << 28) return 0; const ptr = host.alloc(n); host.zeroBytes(ptr, n); return host.newRes({ type: 'buffer', size: n, ptr, onFree() { host.arena.release(ptr); } }); },
    IsBuffer(tid, r) { return host.getRes(r, 'buffer') ? 1 : 0; },
    Describe(tid, r, p) { const b = host.getRes(r, 'buffer'); if (!b) return 0; host.w32(p, b.size); return 1; },
    Map(tid, r) { return host.getRes(r, 'buffer')?.ptr ?? 0; },
    Unmap() {},
  };
  impl.PPB_CharSet = {
    UTF16ToCharSet(tid, instance, utf16, len, charsetPtr, onError, outLenPtr) {
      const cs = host.cstr(charsetPtr).toLowerCase();
      const u16 = new Uint16Array(host.ro(utf16, len * 2).slice().buffer);
      const str = String.fromCharCode(...u16);
      let out;
      if (cs === 'utf-8' || cs === 'utf8') out = new TextEncoder().encode(str);
      else { out = new Uint8Array(str.length); for (let i = 0; i < str.length; i++) out[i] = str.charCodeAt(i) < 256 ? str.charCodeAt(i) : (onError === 0 ? 63 : 63); }
      const p = host.alloc(out.length + 1); host.putBytes(p, out); host.putBytes(p + out.length, new Uint8Array(1));
      host.w32(outLenPtr, out.length); return p;
    },
    CharSetToUTF16(tid, instance, input, len, charsetPtr, onError, outLenPtr) {
      const cs = host.cstr(charsetPtr).toLowerCase();
      const bytes = host.ro(input, len).slice();
      let str;
      try { str = new TextDecoder(cs === 'iso-8859-1' ? 'windows-1252' : cs).decode(bytes); } catch { str = new TextDecoder().decode(bytes); }
      const p = host.alloc(str.length * 2 + 2);
      for (let i = 0; i < str.length; i++) { host.g._dv.setUint16(host._base + p + i * 2, str.charCodeAt(i), true); }
      host.g._dv.setUint16(host._base + p + str.length * 2, 0, true);
      host.w32(outLenPtr, str.length); return p;
    },
    GetDefaultCharSet() { return host.mkVar('utf-8'); },
  };
  impl.PPB_TextInput = { SetTextInputType() {}, UpdateCaretPosition() {}, CancelCompositionText() {}, UpdateSurroundingText() {}, SelectionChanged() {} };
  impl.PPB_MouseLock = { LockMouse() { return PP_ERROR_NOTSUPPORTED; }, UnlockMouse() {} };
  // PP_GamepadsSampleData (1896 bytes): u32 length @0, then 4 items of 472 bytes at 8: u32 axes_length @0, f32 axes[16] @4, u32 buttons_length @68,
  // f32 buttons[32] @72, f64 timestamp @200, u16 id[128] @208, i32 connected @464. host.gamepads is the frame's latest sample (see frame-services.js).
  impl.PPB_Gamepad = {
    Sample(tid, instance, p) {
      const pads = host.gamepads || [];
      host.zeroBytes(p, 1896);
      host.w32(p, pads.length);
      pads.forEach((g, i) => {
        const o = p + 8 + i * 472;
        host.w32(o, g.axes.length); g.axes.forEach((v, k) => host.wf32(o + 4 + k * 4, v));
        host.w32(o + 68, g.buttons.length); g.buttons.forEach((v, k) => host.wf32(o + 72 + k * 4, v));
        host.wf64(o + 200, (g.timestamp || 0) / 1000);
        const id = String(g.id).slice(0, 127);
        for (let k = 0; k < id.length; k++) host.bytesW(o + 208 + k * 2, 2).set([id.charCodeAt(k) & 255, id.charCodeAt(k) >> 8]);
        host.w32(o + 464, 1);
      });
    },
  };
  const CURSORS = ['default', 'crosshair', 'pointer', 'text', 'wait', 'help', 'e-resize', 'ne-resize', 'nw-resize', 'n-resize', 'se-resize', 'sw-resize', 's-resize', 'w-resize', 'ns-resize', 'ew-resize', 'nesw-resize', 'nwse-resize', 'col-resize', 'row-resize', 'n-resize', 'e-resize', 's-resize', 'w-resize', 'move', 'text', 'default', 'default', 'default', 'default', 'default', 'default', 'default', 'default', 'default', 'default', 'default', 'default', 'none'];
  impl.PPB_CursorControl = {
    SetCursor(tid, instance, type, image, hot) { host.onCursor(type === 38 ? 'none' : CURSORS[type] || 'default'); return 1; },
    LockCursor() { return 0; }, UnlockCursor() { return 0; }, HasCursorLock() { return 0; }, CanLockCursor() { return 0; },
  };

  // PPB_Flash_MessageLoop: nested loop on the calling thread (Run is implemented in the guest).
  impl.PPB_Flash_MessageLoop = {
    Create(tid, instance) { return host.newRes({ type: 'flashloop', quit: false }); },
    IsFlashMessageLoop(tid, r) { return host.getRes(r, 'flashloop') ? 1 : 0; },
    Run() { return PP_ERROR_FAILED; },
    Quit(tid, r) { const o = host.getRes(r, 'flashloop'); if (!o) return; o.quit = true; const l = host.loops.get(o.parent); if (l) host.pump(l); },
  };

  impl.PPB_Flash_Print = { InvokePrinting() {} };
  impl.PPB_NetworkMonitor = { Create() { return 0; }, UpdateNetworkList() { return PP_ERROR_NOTSUPPORTED; }, IsNetworkMonitor() { return 0; } };
  return impl;
}
