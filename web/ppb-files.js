// File dialogs for flash.net.FileReference: PPB_FileChooser(Dev) / PPB_FileChooserTrusted (the dialog), PPB_FileRef (what it returns) and
// PPB_Flash_File_FileRef (how Flash then reads or writes the file: it asks for a native file descriptor, i.e. a guest fd here).
//
//   open  the player frame shows the browser's file picker ('pick-files' service, frame-services.js) and the picked bytes are written into the
//         guest file system under FILES_ROOT; OpenFile gives Flash a read-only descriptor to that copy.
//   save  the browser decides where a download goes, so the "dialog" only hands out an empty guest file; once Flash has written it and the size has
//         stopped changing, its bytes are sent to the frame ('save-file' service), which downloads them.
// Nothing outside FILES_ROOT is reachable: a picked file's real path never reaches the guest, only its name and bytes.
import { PP_OK, PP_OK_COMPLETIONPENDING, PP_ERROR_FAILED, PP_ERROR_NOTSUPPORTED, PP_ERROR_BADRESOURCE, PP_ERROR_BLOCKS_MAIN_THREAD, PP_ERROR_FILENOTFOUND, PP_ERROR_NOACCESS } from './pp-consts.js';

const PP_ERROR_USERCANCEL = -40, PP_ERROR_NO_USER_GESTURE = -41;
const PP_FILESYSTEMTYPE_EXTERNAL = 3;
const FILES_ROOT = '/tmp/flash-runner-files';
const O_RDONLY = 0, O_WRONLY = 1, O_RDWR = 2, O_CREAT = 0x40, O_EXCL = 0x80, O_TRUNC = 0x200;
const SAVE_QUIET_MS = 800;             // a saved file is downloaded once its size has been unchanged for this long
const SAVE_WATCH_MS = 10 * 60 * 1000;  // and a save that never gets written is forgotten after this long
export const baseName = (n) => String(n || '').split(/[\\/]/).pop().replace(/[\u0000-\u001f]/g, '_').slice(0, 200);

export function buildFiles(host) {
  const impl = {};
  const loopOf = (tid) => host.threadLoop.get(tid) || 1;
  const u32le = (n) => new Uint8Array([n & 255, (n >> 8) & 255, (n >> 16) & 255, (n >> 24) & 255]);
  // the worker's frameService resolves { ok, value | error }; here a rejection carries the error text
  const service = (op, args) => (host.opts.service ? host.opts.service.request(op, args).then((r) => (r.ok ? r.value : Promise.reject(new Error(r.error)))) : Promise.reject(new Error('no file dialogs here')));
  let nextId = 0;

  // ---- the dialog -----------------------------------------------------------------------------------------
  const chooser = (r) => host.getRes(r, 'filechooser');
  /** A FileRef resource for one file in the guest: dir/name under FILES_ROOT. */
  const newFile = (kind, name) => {
    const dir = `${FILES_ROOT}/${++nextId}`, path = `${dir}/${name}`;
    host.emu.mkdirp(dir);
    return { id: host.newRes({ type: 'fileref', kind, name, dir, path }), path, dir };
  };

  /** Run `pick` (a promise of FileRef resource ids) and fill the caller's PP_ArrayOutput with the result; cancel -> PP_ERROR_USERCANCEL. */
  function showThen(tid, out, cc, pick) {
    if (!cc.func && tid === host.mainTid) return PP_ERROR_BLOCKS_MAIN_THREAD;
    const done = pick.then((ids) => { host.log(`[file] dialog gave ${ids.length} file(s)`); return host.arrayThenCallback(tid, out, 4, ids.map(u32le), cc, ids.length ? PP_OK : PP_ERROR_USERCANCEL); },
      (e) => { host.log(`[file] dialog refused: ${e && e.message}`); return host.arrayThenCallback(tid, out, 4, [], cc, /activation|click or key/.test(String(e && e.message)) ? PP_ERROR_NO_USER_GESTURE : PP_ERROR_FAILED); });
    return cc.func ? PP_OK_COMPLETIONPENDING : done;
  }
  const open = (c) => service('pick-files', { accept: c.accept, multiple: c.mode === 1 }).then((files) => files.map((f) => {
    const name = baseName(f.name) || 'file', ref = newFile('open', name);
    host.emu.writeFile(ref.path, f.bytes);
    return ref.id;
  }));
  const showTrusted = (tid, c, saveAs, suggested, out, cc) => {
    if (!c) return PP_ERROR_BADRESOURCE;
    if (!saveAs) return showThen(tid, out, cc, open(c));
    const ref = newFile('save', baseName(host.varValue(suggested)) || 'download');   // the download happens when Flash has written the file (watchSave)
    return showThen(tid, out, cc, Promise.resolve([ref.id]));
  };

  const chooserImpl = {
    Create(tid, instance, mode, accept) { const a = host.varValue(accept); return host.newRes({ type: 'filechooser', instance, mode, accept: typeof a === 'string' ? a : '' }); },
    IsFileChooser(tid, r) { return chooser(r) ? 1 : 0; },
    Show(tid, r, ...rest) {
      const c = chooser(r), cc = rest[rest.length - 1], out = rest.length > 1 ? rest[0] : null;
      if (!c) return PP_ERROR_BADRESOURCE;
      if (out) return showThen(tid, out, cc, open(c));
      // 0.5: no output array, the files are fetched one by one with GetNextChosenFile
      c.queue = [];
      if (!cc.func && tid === host.mainTid) return PP_ERROR_BLOCKS_MAIN_THREAD;
      const p = open(c).then((ids) => { c.queue = ids; return ids.length ? PP_OK : PP_ERROR_USERCANCEL; }, () => PP_ERROR_FAILED);
      if (!cc.func) return p;
      p.then((res) => host.postTask(loopOf(tid), cc.func, [cc.user, res >>> 0]));
      return PP_OK_COMPLETIONPENDING;
    },
    GetNextChosenFile(tid, r) { return chooser(r)?.queue?.shift() || 0; },
  };
  impl['PPB_FileChooser(Dev)'] = chooserImpl;
  impl.PPB_FileChooser = chooserImpl;
  impl.PPB_FileChooserTrusted = {
    ShowWithoutUserGesture(tid, r, saveAs, suggested, ...rest) {
      const cc = rest[rest.length - 1], out = rest.length > 1 ? rest[0] : null;
      if (out) return showTrusted(tid, chooser(r), !!saveAs, suggested, out, cc);
      return chooserImpl.Show(tid, r, cc);
    },
  };

  // ---- file references and the descriptor Flash opens on them ---------------------------------------------
  const ref = (r) => host.getRes(r, 'fileref');
  const writeFileInfo = (p, st) => {
    host.w32(p, st.size >>> 0); host.w32(p + 4, Math.floor(st.size / 2 ** 32));
    host.wi32(p + 8, 0);                                   // PP_FILETYPE_REGULAR
    host.wi32(p + 12, PP_FILESYSTEMTYPE_EXTERNAL);
    for (const o of [16, 24, 32]) host.wf64(p + o, st.mtime);
  };
  /** Download a saved file when Flash has finished writing it: size > 0 and unchanged for SAVE_QUIET_MS. Re-armed if Flash writes it again. */
  function watchSave(r) {
    if (r.watching) return; r.watching = true;
    const started = Date.now(); let last = -1, since = Date.now();
    const tick = () => {
      if (Date.now() - started > SAVE_WATCH_MS || !host.emu) { r.watching = false; return; }
      const st = host.emu.stat(r.path);
      if (st && st.size !== last) { last = st.size; since = Date.now(); }
      if (st && st.size > 0 && Date.now() - since >= SAVE_QUIET_MS && st.size !== r.savedSize) {
        r.savedSize = st.size;
        const file = host.emu.dumpTree(r.dir).find(([p]) => p === r.path);
        if (file) service('save-file', { name: r.name, bytes: file[1] }).then(() => host.log(`[file] saved ${r.name} (${file[1].length} bytes)`), (e) => host.log(`[file] save failed: ${e && e.message}`));
      }
      setTimeout(tick, 250);
    };
    setTimeout(tick, 250);
  }
  impl.PPB_FileRef = {
    Create() { return 0; },                                // no file system to refer into
    IsFileRef(tid, r) { return ref(r) ? 1 : 0; },
    GetFileSystemType(tid, r) { return ref(r) ? PP_FILESYSTEMTYPE_EXTERNAL : 0; },
    GetName(tid, r) { return host.mkVar(ref(r)?.name ?? undefined); },
    GetPath(tid, r) { return host.mkVar(undefined); },     // an external file has no path in a PPAPI file system
    GetParent() { return 0; },
    MakeDirectory(tid, ...a) { return host.complete(tid, a[a.length - 1], PP_ERROR_NOTSUPPORTED); },
    Touch(tid, ...a) { return host.complete(tid, a[a.length - 1], PP_OK); },
    Delete(tid, ...a) { return host.complete(tid, a[a.length - 1], PP_ERROR_NOTSUPPORTED); },
    Rename(tid, ...a) { return host.complete(tid, a[a.length - 1], PP_ERROR_NOTSUPPORTED); },
    Query(tid, r, info, cc) {
      const f = ref(r), st = f && host.emu.stat(f.path);
      if (!st) return host.complete(tid, cc, f ? PP_ERROR_FILENOTFOUND : PP_ERROR_BADRESOURCE);
      writeFileInfo(info, st); return host.complete(tid, cc, PP_OK);
    },
    ReadDirectoryEntries(tid, ...a) { return host.complete(tid, a[a.length - 1], PP_ERROR_NOTSUPPORTED); },
  };
  impl.PPB_FileRefPrivate = { GetAbsolutePath(tid, r) { return host.mkVar(ref(r)?.name ?? undefined); } };
  impl.PPB_Flash_File_FileRef = {
    OpenFile(tid, r, mode, filePtr) {
      const f = ref(r); if (!f) return PP_ERROR_BADRESOURCE;
      let fl = (mode & 3) === 3 ? O_RDWR : (mode & 2) ? O_WRONLY : O_RDONLY;
      if (f.kind === 'open' && fl !== O_RDONLY) return PP_ERROR_NOACCESS;          // a picked file is a copy to read, not something to overwrite
      if (f.kind === 'save') { if (fl === O_RDONLY) fl = O_RDWR; fl |= O_CREAT; if (mode & 8) fl |= O_TRUNC; if (mode & 16) fl |= O_EXCL; }
      const fd = host.emu.openFd(f.path, fl);
      if (fd < 0) { host.wi32(filePtr, -1); return fd === -2 ? PP_ERROR_FILENOTFOUND : PP_ERROR_FAILED; }
      host.wi32(filePtr, fd);
      if (f.kind === 'save') watchSave(f);
      return PP_OK;
    },
    QueryFile(tid, r, info) {
      const f = ref(r), st = f && host.emu.stat(f.path);
      if (!st) return f ? PP_ERROR_FILENOTFOUND : PP_ERROR_BADRESOURCE;
      writeFileInfo(info, st); return PP_OK;
    },
  };
  return impl;
}
