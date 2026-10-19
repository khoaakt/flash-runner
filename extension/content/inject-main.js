// Runs in the page's own JavaScript world at document_start. Makes Flash detection scripts (swfobject,
// hand-rolled navigator.plugins checks) believe a Flash Player 32 plugin is installed, so sites emit
// <object>/<embed> tags that the content script can then take over.
(() => {
  if (window.__flashRunnerSpoof) return;
  window.__flashRunnerSpoof = true;
  const nav = Navigator.prototype;
  const realPlugins = Object.getOwnPropertyDescriptor(nav, 'plugins')?.get;
  const realMimes = Object.getOwnPropertyDescriptor(nav, 'mimeTypes')?.get;
  if (!realPlugins || !realMimes) return;

  const MIME_FLASH = 'application/x-shockwave-flash';
  const MIME_SPL = 'application/futuresplash';
  const plugin = { name: 'Shockwave Flash', filename: 'pepflashplayer.dll', description: 'Shockwave Flash 32.0 r0' };
  const mimes = [
    { type: MIME_FLASH, suffixes: 'swf', description: 'Shockwave Flash' },
    { type: MIME_SPL, suffixes: 'spl', description: 'FutureSplash Player' },
  ];

  const mkMime = (m) => { const o = Object.create(MimeType.prototype); Object.defineProperties(o, { type: { value: m.type, enumerable: true }, suffixes: { value: m.suffixes, enumerable: true }, description: { value: m.description, enumerable: true }, enabledPlugin: { get: () => flash, enumerable: true } }); return o; };
  const flash = Object.create(Plugin.prototype);
  const flashMimes = mimes.map(mkMime);
  Object.defineProperties(flash, {
    name: { value: plugin.name, enumerable: true }, filename: { value: plugin.filename, enumerable: true },
    description: { value: plugin.description, enumerable: true }, length: { value: flashMimes.length, enumerable: true },
    item: { value: (i) => flashMimes[i] || null }, namedItem: { value: (n) => flashMimes.find((m) => m.type === n) || null },
  });
  flashMimes.forEach((m, i) => Object.defineProperty(flash, i, { value: m, enumerable: true }));

  const wrap = (real, extra, proto, key) => {
    let cache = null, last = null;
    return function () {
      const list = real.call(this);
      if (cache && last === list) return cache;
      const items = [...list, ...extra];
      const o = Object.create(proto);
      Object.defineProperty(o, 'length', { value: items.length });
      items.forEach((it, i) => Object.defineProperty(o, i, { value: it, enumerable: true }));
      items.forEach((it) => { const n = it[key]; if (n && !(n in o)) Object.defineProperty(o, n, { value: it }); });
      Object.defineProperties(o, {
        item: { value: (i) => items[i] || null }, namedItem: { value: (n) => items.find((it) => it[key] === n) || null },
        refresh: { value: () => {} }, [Symbol.iterator]: { value: function* () { yield* items; } },
      });
      last = list; cache = o; return o;
    };
  };
  try {
    Object.defineProperty(nav, 'plugins', { get: wrap(realPlugins, [flash], PluginArray.prototype, 'name'), configurable: true, enumerable: true });
    Object.defineProperty(nav, 'mimeTypes', { get: wrap(realMimes, flashMimes, MimeTypeArray.prototype, 'type'), configurable: true, enumerable: true });
  } catch (e) { /* locked-down page: leave detection alone */ }
})();
