//! In-memory Linux-style filesystem. The read-only base comes from a host directory (native tests
//! and tools) or a tar image (browser); everything written lands in memory. Device nodes
//! (`/dev/null`, `/dev/urandom`, ...) are special inodes serviced by the kernel layer.

use std::collections::BTreeMap;

pub const S_IFMT: u32 = 0o170000;
pub const S_IFDIR: u32 = 0o040000;
pub const S_IFREG: u32 = 0o100000;
pub const S_IFLNK: u32 = 0o120000;
pub const S_IFCHR: u32 = 0o020000;

pub const ENOENT: i32 = 2;
pub const EACCES: i32 = 13;
pub const EEXIST: i32 = 17;
pub const ENOTDIR: i32 = 20;
pub const EISDIR: i32 = 21;
pub const EINVAL: i32 = 22;
pub const ELOOP: i32 = 40;
pub const ENOTEMPTY: i32 = 39;

#[derive(Clone, Debug)]
pub enum Kind {
    File(Vec<u8>),
    Dir(BTreeMap<String, u32>),
    Symlink(String),
    /// (major, minor)
    CharDev(u32, u32),
}

#[derive(Clone, Debug)]
pub struct Inode {
    pub kind: Kind,
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub mtime: i64,
    pub nlink: u32,
}

#[derive(Clone, Debug)]
pub struct Stat {
    pub ino: u32,
    pub mode: u32,
    pub nlink: u32,
    pub uid: u32,
    pub gid: u32,
    pub size: u64,
    pub mtime: i64,
    pub rdev: (u32, u32),
}

pub struct Vfs {
    pub inodes: Vec<Option<Inode>>,
    pub cwd: String,
}

impl Vfs {
    pub fn new() -> Vfs {
        let mut v = Vfs { inodes: Vec::new(), cwd: "/".into() };
        v.inodes.push(None); // ino 0 unused
        v.alloc(Inode { kind: Kind::Dir(BTreeMap::new()), mode: S_IFDIR | 0o755, uid: 0, gid: 0, mtime: 0, nlink: 2 });
        for (name, maj, min) in [("null", 1, 3), ("zero", 1, 5), ("random", 1, 8), ("urandom", 1, 9), ("tty", 5, 0), ("hwrng", 10, 183)] {
            let _ = v.mkdir_p("/dev");
            let ino = v.alloc(Inode { kind: Kind::CharDev(maj, min), mode: S_IFCHR | 0o666, uid: 0, gid: 0, mtime: 0, nlink: 1 });
            v.link_into("/dev", name, ino);
        }
        let _ = v.mkdir_p("/tmp");
        v
    }

    pub fn alloc(&mut self, i: Inode) -> u32 {
        self.inodes.push(Some(i));
        (self.inodes.len() - 1) as u32
    }

    pub fn get(&self, ino: u32) -> Option<&Inode> {
        self.inodes.get(ino as usize).and_then(|x| x.as_ref())
    }
    pub fn get_mut(&mut self, ino: u32) -> Option<&mut Inode> {
        self.inodes.get_mut(ino as usize).and_then(|x| x.as_mut())
    }

    fn link_into(&mut self, dir: &str, name: &str, ino: u32) {
        if let Ok(d) = self.lookup(dir, true) {
            if let Some(Inode { kind: Kind::Dir(m), .. }) = self.get_mut(d) {
                m.insert(name.to_string(), ino);
            }
        }
    }

    pub fn normalize(&self, path: &str) -> String {
        let full = if path.starts_with('/') { path.to_string() } else { format!("{}/{}", self.cwd, path) };
        let mut out: Vec<&str> = Vec::new();
        for c in full.split('/') {
            match c {
                "" | "." => {}
                ".." => {
                    out.pop();
                }
                c => out.push(c),
            }
        }
        format!("/{}", out.join("/"))
    }

    /// Resolve to an inode, following symlinks in every component (and the last if `follow`).
    pub fn lookup(&self, path: &str, follow: bool) -> Result<u32, i32> {
        self.lookup_depth(path, follow, 0)
    }

    fn lookup_depth(&self, path: &str, follow: bool, depth: u32) -> Result<u32, i32> {
        if depth > 16 {
            return Err(ELOOP);
        }
        let norm = self.normalize(path);
        let comps: Vec<&str> = norm.split('/').filter(|c| !c.is_empty()).collect();
        let mut cur = 1u32;
        let mut walked = String::new();
        for (i, c) in comps.iter().enumerate() {
            let last = i == comps.len() - 1;
            let dir = match self.get(cur) {
                Some(Inode { kind: Kind::Dir(m), .. }) => m,
                _ => return Err(ENOTDIR),
            };
            let ino = *dir.get(*c).ok_or(ENOENT)?;
            walked.push('/');
            walked.push_str(c);
            if let Some(Inode { kind: Kind::Symlink(t), .. }) = self.get(ino) {
                if !last || follow {
                    let parent = walked.rsplit_once('/').map(|x| if x.0.is_empty() { "/" } else { x.0 }).unwrap_or("/");
                    let target = if t.starts_with('/') { t.clone() } else { format!("{}/{}", parent, t) };
                    let rest: Vec<&str> = comps[i + 1..].to_vec();
                    let joined = if rest.is_empty() { target } else { format!("{}/{}", target, rest.join("/")) };
                    return self.lookup_depth(&joined, follow, depth + 1);
                }
            }
            cur = ino;
        }
        Ok(cur)
    }

    pub fn stat_ino(&self, ino: u32) -> Option<Stat> {
        let i = self.get(ino)?;
        let (size, rdev) = match &i.kind {
            Kind::File(d) => (d.len() as u64, (0, 0)),
            Kind::Dir(m) => (m.len() as u64 * 32 + 4096, (0, 0)),
            Kind::Symlink(t) => (t.len() as u64, (0, 0)),
            Kind::CharDev(a, b) => (0, (*a, *b)),
        };
        Some(Stat { ino, mode: i.mode, nlink: i.nlink, uid: i.uid, gid: i.gid, size, mtime: i.mtime, rdev })
    }

    pub fn mkdir_p(&mut self, path: &str) -> Result<u32, i32> {
        let norm = self.normalize(path);
        let mut cur = 1u32;
        for c in norm.split('/').filter(|c| !c.is_empty()) {
            let existing = match self.get(cur) {
                Some(Inode { kind: Kind::Dir(m), .. }) => m.get(c).copied(),
                _ => return Err(ENOTDIR),
            };
            cur = match existing {
                Some(i) => i,
                None => {
                    let n = self.alloc(Inode { kind: Kind::Dir(BTreeMap::new()), mode: S_IFDIR | 0o755, uid: 0, gid: 0, mtime: 0, nlink: 2 });
                    if let Some(Inode { kind: Kind::Dir(m), .. }) = self.get_mut(cur) {
                        m.insert(c.to_string(), n);
                    }
                    n
                }
            };
        }
        Ok(cur)
    }

    fn split_parent(&self, path: &str) -> (String, String) {
        let norm = self.normalize(path);
        match norm.rsplit_once('/') {
            Some((p, n)) => (if p.is_empty() { "/".into() } else { p.to_string() }, n.to_string()),
            None => ("/".into(), norm),
        }
    }

    pub fn create_file(&mut self, path: &str, data: Vec<u8>, mode: u32) -> Result<u32, i32> {
        let (dir, name) = self.split_parent(path);
        let d = self.mkdir_p(&dir)?;
        let ino = self.alloc(Inode { kind: Kind::File(data), mode: S_IFREG | (mode & 0o7777), uid: 0, gid: 0, mtime: 0, nlink: 1 });
        if let Some(Inode { kind: Kind::Dir(m), .. }) = self.get_mut(d) {
            m.insert(name, ino);
        }
        Ok(ino)
    }

    pub fn create_symlink(&mut self, path: &str, target: &str) -> Result<u32, i32> {
        let (dir, name) = self.split_parent(path);
        let d = self.mkdir_p(&dir)?;
        let ino = self.alloc(Inode { kind: Kind::Symlink(target.to_string()), mode: S_IFLNK | 0o777, uid: 0, gid: 0, mtime: 0, nlink: 1 });
        if let Some(Inode { kind: Kind::Dir(m), .. }) = self.get_mut(d) {
            m.insert(name, ino);
        }
        Ok(ino)
    }

    pub fn unlink(&mut self, path: &str) -> Result<(), i32> {
        let (dir, name) = self.split_parent(path);
        let d = self.lookup(&dir, true)?;
        if let Some(Inode { kind: Kind::Dir(m), .. }) = self.get_mut(d) {
            m.remove(&name).map(|_| ()).ok_or(ENOENT)
        } else {
            Err(ENOTDIR)
        }
    }

    pub fn mkdir(&mut self, path: &str, mode: u32) -> Result<(), i32> {
        if self.lookup(path, false).is_ok() {
            return Err(EEXIST);
        }
        let (dir, name) = self.split_parent(path);
        let d = self.lookup(&dir, true)?;
        let n = self.alloc(Inode { kind: Kind::Dir(BTreeMap::new()), mode: S_IFDIR | (mode & 0o7777), uid: 0, gid: 0, mtime: 0, nlink: 2 });
        if let Some(Inode { kind: Kind::Dir(m), .. }) = self.get_mut(d) {
            m.insert(name, n);
            Ok(())
        } else {
            Err(ENOTDIR)
        }
    }

    pub fn rename(&mut self, from: &str, to: &str) -> Result<(), i32> {
        let ino = self.lookup(from, false)?;
        let (dir, name) = self.split_parent(to);
        let d = self.lookup(&dir, true)?; // validate the destination before touching the source
        self.unlink(from)?;
        if let Some(Inode { kind: Kind::Dir(m), .. }) = self.get_mut(d) {
            m.insert(name, ino);
            Ok(())
        } else {
            Err(ENOTDIR)
        }
    }

    /// Populate from a USTAR/GNU tar image (regular files, dirs, symlinks, hardlinks as copies).
    pub fn load_tar(&mut self, tar: &[u8]) {
        let mut off = 0usize;
        let mut long_name: Option<String> = None;
        let mut long_link: Option<String> = None;
        let mut hard: Vec<(String, String)> = Vec::new();
        while off + 512 <= tar.len() {
            let h = &tar[off..off + 512];
            if h.iter().all(|&b| b == 0) {
                break;
            }
            let field = |a: usize, b: usize| -> &[u8] {
                let s = &h[a..b];
                let n = s.iter().position(|&c| c == 0).unwrap_or(s.len());
                &s[..n]
            };
            let oct = |a: usize, b: usize| -> u64 { u64::from_str_radix(std::str::from_utf8(field(a, b)).unwrap_or("0").trim(), 8).unwrap_or(0) };
            let mut name = String::from_utf8_lossy(field(0, 100)).into_owned();
            let prefix = String::from_utf8_lossy(field(345, 500)).into_owned();
            if !prefix.is_empty() {
                name = format!("{}/{}", prefix, name);
            }
            let size = oct(124, 136) as usize;
            let mode = oct(100, 108) as u32;
            let mtime = oct(136, 148) as i64;
            let typ = h[156];
            let link = String::from_utf8_lossy(field(157, 257)).into_owned();
            let data_off = off + 512;
            let blob = tar.get(data_off..data_off + size).unwrap_or(&[]);
            off = data_off + ((size + 511) & !511);
            match typ {
                b'L' => {
                    long_name = Some(String::from_utf8_lossy(blob).trim_end_matches('\0').to_string());
                    continue;
                }
                b'K' => {
                    long_link = Some(String::from_utf8_lossy(blob).trim_end_matches('\0').to_string());
                    continue;
                }
                _ => {}
            }
            if let Some(n) = long_name.take() {
                name = n;
            }
            let link = long_link.take().unwrap_or(link);
            let path = format!("/{}", name.trim_start_matches("./").trim_end_matches('/'));
            match typ {
                b'0' | 0 => {
                    if let Ok(i) = self.create_file(&path, blob.to_vec(), mode) {
                        if let Some(n) = self.get_mut(i) {
                            n.mtime = mtime;
                        }
                    }
                }
                b'5' => {
                    let _ = self.mkdir_p(&path);
                }
                b'2' => {
                    let _ = self.create_symlink(&path, &link);
                }
                b'1' => hard.push((path, format!("/{}", link.trim_start_matches("./")))),
                _ => {}
            }
        }
        for (path, target) in hard {
            if let Ok(t) = self.lookup(&target, true) {
                if let Some(Inode { kind: Kind::File(d), mode, .. }) = self.get(t).cloned() {
                    let _ = self.create_file(&path, d, mode);
                }
            }
        }
    }

    /// Native only: mirror a host directory tree into memory.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn load_host_dir(&mut self, root: &std::path::Path) -> std::io::Result<()> {
        // Windows hosts carry no Unix mode: files are loaded executable so a guest binary stays runnable.
        #[cfg(unix)]
        fn host_mode(md: &std::fs::Metadata) -> u32 { use std::os::unix::fs::PermissionsExt; md.permissions().mode() }
        #[cfg(not(unix))]
        fn host_mode(_: &std::fs::Metadata) -> u32 { 0o755 }
        fn walk(v: &mut Vfs, host: &std::path::Path, guest: &str) -> std::io::Result<()> {
            for e in std::fs::read_dir(host)? {
                let e = e?;
                let name = e.file_name().to_string_lossy().into_owned();
                let gpath = if guest == "/" { format!("/{}", name) } else { format!("{}/{}", guest, name) };
                let md = std::fs::symlink_metadata(e.path())?;
                if md.file_type().is_symlink() {
                    let t = std::fs::read_link(e.path())?;
                    let _ = v.create_symlink(&gpath, &t.to_string_lossy());
                } else if md.is_dir() {
                    let _ = v.mkdir_p(&gpath);
                    walk(v, &e.path(), &gpath)?;
                } else if md.is_file() {
                    let _ = v.create_file(&gpath, std::fs::read(e.path())?, host_mode(&md));
                }
            }
            Ok(())
        }
        walk(self, root, "/")
    }
}
