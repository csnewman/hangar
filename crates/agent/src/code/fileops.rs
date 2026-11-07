//! What the Project tree does to files besides creating, renaming and
//! deleting them one at a time: copying and moving several, as IntelliJ's
//! paste and drag do; putting them in the trash and back, which is how a
//! deletion is undone; taking uploads; packing a download; and changing a
//! file's permissions, line separators, encoding and byte order mark.
//!
//! These are blocking: callers run them on the blocking pool.

use std::collections::HashMap;
use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use super::error::{Error, Result, at, between};

// --- Copying and moving ------------------------------------------------------

/// What a copy or move does with a name the folder already has: report it
/// and do nothing (`Ask`, so the user can choose), put the new one in its
/// place, leave that one out, or give it a free name beside it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Clash {
    Ask,
    Replace,
    Skip,
    Rename,
}

impl Clash {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "ask" => Clash::Ask,
            "replace" => Clash::Replace,
            "skip" => Clash::Skip,
            "rename" => Clash::Rename,
            _ => return None,
        })
    }
}

pub struct TransferResult {
    /// What went where; nothing for `Ask` with clashes.
    pub done: Vec<(PathBuf, PathBuf)>,
    /// The names the folder already has, for `Ask`.
    pub clashes: Vec<String>,
}

fn base(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// A name's extension as Node's extname has it: none for a name that is all
/// extension, like ".bashrc".
fn ext(name: &str) -> &str {
    match name.rfind('.') {
        Some(0) | None => "",
        Some(i) => &name[i..],
    }
}

/// A name for `name` in `dir` that nothing there has: the name itself, else
/// "name copy", "name copy 2", ..., the extension kept.
pub fn free_name(dir: &Path, name: &str) -> String {
    if !exists(&dir.join(name)) {
        return name.to_owned();
    }
    let e = ext(name);
    let stem = &name[..name.len() - e.len()];
    (1..)
        .map(|i| {
            if i > 1 {
                format!("{stem} copy {i}{e}")
            } else {
                format!("{stem} copy{e}")
            }
        })
        .find(|n| !exists(&dir.join(n)))
        .expect("a free name")
}

fn exists(p: &Path) -> bool {
    fs::symlink_metadata(p).is_ok()
}

/// Copies or moves each of `from` (absolute) into the folder `to`, under
/// its own name or, for one, the name given. A folder is not put inside
/// itself, and a move to where a thing already is does nothing.
pub fn transfer(
    move_: bool,
    from: &[PathBuf],
    to: &Path,
    clash: Clash,
    name: Option<&str>,
) -> Result<TransferResult> {
    if !fs::metadata(to).map_err(at(to))?.is_dir() {
        return Err(Error::msg(format!("{} is not a folder", base(to))));
    }
    if let Some(n) = name
        && (from.len() != 1 || !valid_name(n))
    {
        return Err(Error::msg(format!("not a valid name: {n}")));
    }
    let mut plan: Vec<(PathBuf, PathBuf, bool)> = Vec::new();
    for f in from {
        if to.starts_with(f) {
            return Err(Error::msg(format!("cannot put {} inside itself", base(f))));
        }
        let target = to.join(name.map(str::to_owned).unwrap_or_else(|| base(f)));
        if &target == f {
            // Onto itself: a move stays, a copy is a duplicate beside it.
            if move_ {
                continue;
            }
            plan.push((f.clone(), to.join(free_name(to, &base(&target))), false));
            continue;
        }
        let clashes = exists(&target);
        plan.push((f.clone(), target, clashes));
    }
    let clashes: Vec<String> = plan.iter().filter(|p| p.2).map(|p| base(&p.1)).collect();
    if clash == Clash::Ask && !clashes.is_empty() {
        return Ok(TransferResult {
            done: Vec::new(),
            clashes,
        });
    }
    let mut done = Vec::new();
    for (src, mut target, clashed) in plan {
        if clashed {
            match clash {
                Clash::Skip => continue,
                Clash::Rename => target = to.join(free_name(to, &base(&target))),
                _ => remove_all(&target)?,
            }
        }
        if move_ {
            mv(&src, &target)?;
        } else {
            copy_tree(&src, &target)?;
        }
        done.push((src, target));
    }
    Ok(TransferResult {
        done,
        clashes: Vec::new(),
    })
}

/// Renames, or copies and deletes across file systems.
fn mv(from: &Path, to: &Path) -> Result<()> {
    match fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(e) if e.raw_os_error() == Some(libc::EXDEV) => {
            copy_tree(from, to)?;
            remove_all(from)
        }
        Err(e) => Err(between(from, to)(e)),
    }
}

/// Copies a file, folder or link, keeping links as the names they hold,
/// modes and modification times; nothing there already is replaced.
fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    let m = fs::symlink_metadata(from).map_err(at(from))?;
    if m.file_type().is_symlink() {
        let target = fs::read_link(from).map_err(at(from))?;
        std::os::unix::fs::symlink(target, to).map_err(between(from, to))?;
    } else if m.is_dir() {
        fs::create_dir(to).map_err(between(from, to))?;
        for e in fs::read_dir(from).map_err(at(from))? {
            let e = e.map_err(at(from))?;
            copy_tree(&e.path(), &to.join(e.file_name()))?;
        }
        fs::set_permissions(to, fs::Permissions::from_mode(m.mode() & 0o7777)).map_err(at(to))?;
    } else {
        let mut src = fs::File::open(from).map_err(at(from))?;
        let mut dst = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(to)
            .map_err(between(from, to))?;
        io::copy(&mut src, &mut dst).map_err(at(to))?;
        fs::set_permissions(to, fs::Permissions::from_mode(m.mode() & 0o7777)).map_err(at(to))?;
    }
    set_times(to, &m);
    Ok(())
}

fn set_times(p: &Path, m: &fs::Metadata) {
    let Ok(c) = std::ffi::CString::new(p.as_os_str().as_encoded_bytes()) else {
        return;
    };
    let ts = [
        libc::timespec {
            tv_sec: m.atime() as _,
            tv_nsec: m.atime_nsec() as _,
        },
        libc::timespec {
            tv_sec: m.mtime() as _,
            tv_nsec: m.mtime_nsec() as _,
        },
    ];
    // SAFETY: c is a NUL-terminated path and ts two timespecs.
    unsafe {
        libc::utimensat(
            libc::AT_FDCWD,
            c.as_ptr(),
            ts.as_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        );
    }
}

fn remove_all(p: &Path) -> Result<()> {
    match fs::symlink_metadata(p) {
        Err(_) => Ok(()),
        Ok(m) if m.is_dir() => fs::remove_dir_all(p).map_err(at(p)),
        Ok(_) => fs::remove_file(p).map_err(at(p)),
    }
}

fn valid_name(n: &str) -> bool {
    !n.is_empty() && n != "." && n != ".." && !n.contains('/') && !n.contains('\0')
}

// --- The trash ----------------------------------------------------------------

/// The trash is the desktop's, as the freedesktop.org specification has it
/// (~/.local/share/Trash): what is put there is in files/, and in info/
/// where it came from, so a file manager can restore it too.
fn trash_dir() -> PathBuf {
    let data = std::env::var_os("XDG_DATA_HOME")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".local/share"));
    data.join("Trash")
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// Puts each path in the trash, returning where each went: its id there, to
/// restore it by.
pub fn trash(paths: &[PathBuf]) -> Result<Vec<(PathBuf, String)>> {
    let dir = trash_dir();
    fs::create_dir_all(dir.join("files")).map_err(at(&dir))?;
    fs::create_dir_all(dir.join("info")).map_err(at(&dir))?;
    let mut out = Vec::new();
    for p in paths {
        let id = claim_info(&dir, &base(p), p)?;
        if let Err(e) = mv(p, &dir.join("files").join(&id)) {
            let _ = fs::remove_file(dir.join("info").join(format!("{id}.trashinfo")));
            return Err(e);
        }
        out.push((p.clone(), id));
    }
    Ok(out)
}

/// Writes a trashinfo file for a name not yet in the trash, claiming the
/// name, which is the id.
fn claim_info(dir: &Path, name: &str, path: &Path) -> Result<String> {
    let e = ext(name);
    let stem = &name[..name.len() - e.len()];
    let t = local_time(SystemTime::now());
    let date = format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        t.year, t.month, t.day, t.hour, t.minute, t.second
    );
    let encoded: Vec<String> = path
        .display()
        .to_string()
        .split('/')
        .map(encode_uri_component)
        .collect();
    let info = format!(
        "[Trash Info]\nPath={}\nDeletionDate={date}\n",
        encoded.join("/")
    );
    for i in 1.. {
        let id = if i == 1 {
            name.to_owned()
        } else {
            format!("{stem}.{i}{e}")
        };
        let file = dir.join("info").join(format!("{id}.trashinfo"));
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&file)
        {
            Ok(mut f) => f.write_all(info.as_bytes()).map_err(at(&file))?,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(at(&file)(e)),
        }
        if exists(&dir.join("files").join(&id)) {
            continue;
        }
        return Ok(id);
    }
    unreachable!()
}

/// encodeURIComponent's escaping: everything but letters, digits and
/// -_.!~*'() as UTF-8 bytes in %XX.
fn encode_uri_component(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Puts back what `trash` put there. Something there since is not replaced.
pub fn restore(items: &[(PathBuf, String)]) -> Result<()> {
    let dir = trash_dir();
    for (path, id) in items {
        if !valid_name(id) {
            return Err(Error::msg(format!("not in the trash: {id}")));
        }
        if exists(path) {
            return Err(Error::msg(format!(
                "{} is there again; it was not replaced",
                base(path)
            )));
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(at(parent))?;
        }
        mv(&dir.join("files").join(id), path)?;
        let _ = fs::remove_file(dir.join("info").join(format!("{id}.trashinfo")));
    }
    Ok(())
}

// --- Uploads --------------------------------------------------------------------

/// Uploads arrive in parts, written to a hidden file beside where they go
/// and renamed into place once whole, so nothing sees half a file.
#[derive(Default)]
pub struct Uploads {
    open: Mutex<HashMap<String, (PathBuf, PathBuf)>>,
}

impl Uploads {
    pub fn start(&self, path: &Path) -> Result<String> {
        let dir = path.parent().unwrap_or(Path::new("/"));
        fs::create_dir_all(dir).map_err(at(dir))?;
        let token = random_hex(12);
        let tmp = dir.join(format!(".{}.hangar-upload-{token}", base(path)));
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .map_err(at(&tmp))?;
        self.open
            .lock()
            .unwrap()
            .insert(token.clone(), (tmp, path.to_path_buf()));
        Ok(token)
    }

    pub fn write(&self, token: &str, data: &[u8]) -> Result<()> {
        let Some((tmp, _)) = self.open.lock().unwrap().get(token).cloned() else {
            return Err(Error::msg("no such upload"));
        };
        let mut f = fs::OpenOptions::new()
            .append(true)
            .open(&tmp)
            .map_err(at(&tmp))?;
        f.write_all(data).map_err(at(&tmp))
    }

    /// Puts the upload in place, replacing what is there only if asked.
    pub fn end(&self, token: &str, replace: bool) -> Result<PathBuf> {
        let Some((tmp, path)) = self.open.lock().unwrap().remove(token) else {
            return Err(Error::msg("no such upload"));
        };
        if !replace && exists(&path) {
            let _ = fs::remove_file(&tmp);
            return Err(Error::msg(format!("{} already exists", base(&path))));
        }
        if replace {
            remove_all(&path)?;
        }
        fs::rename(&tmp, &path).map_err(between(&tmp, &path))?;
        Ok(path)
    }

    pub fn abort(&self, token: &str) {
        if let Some((tmp, _)) = self.open.lock().unwrap().remove(token) {
            let _ = fs::remove_file(tmp);
        }
    }
}

pub fn random_hex(n: usize) -> String {
    let mut buf = vec![0u8; n];
    if fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut buf))
        .is_err()
    {
        let t = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        for (i, b) in buf.iter_mut().enumerate() {
            *b = (t >> ((i % 16) * 8)) as u8 ^ (std::process::id() as u8);
        }
    }
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

// --- Downloads ------------------------------------------------------------------

fn zip_dir() -> Result<&'static Path> {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    if let Some(d) = DIR.get() {
        return Ok(d);
    }
    let d = std::env::temp_dir().join(format!("hangar-code-zip-{}", random_hex(6)));
    fs::create_dir_all(&d).map_err(at(&d))?;
    Ok(DIR.get_or_init(|| d))
}

/// Whether a path is a download `zip` packed.
pub fn is_packed(p: &Path) -> bool {
    zip_dir().is_ok_and(|d| p.parent() == Some(d))
}

#[derive(Serialize)]
pub struct Packed {
    pub path: String,
    pub size: u64,
}

/// Packs files and folders into a zip file of their contents, each under
/// its own name, for the browser to read and then remove. Links are kept as
/// what they lead to when that is a file, and left out otherwise, so a link
/// to a folder above cannot make it endless.
pub fn zip(paths: &[PathBuf]) -> Result<Packed> {
    let out = zip_dir()?.join(format!("{}.zip", random_hex(8)));
    let file = fs::File::create(&out).map_err(at(&out))?;
    let mut z = ZipWriter {
        out: io::BufWriter::new(file),
        offset: 0,
        entries: Vec::new(),
    };
    let result = (|| {
        for p in paths {
            add_to(&mut z, p, &base(p))?;
        }
        z.finish()
    })();
    if let Err(e) = result {
        let _ = fs::remove_file(&out);
        return Err(e);
    }
    let size = fs::metadata(&out).map_err(at(&out))?.len();
    Ok(Packed {
        path: out.display().to_string(),
        size,
    })
}

fn add_to(z: &mut ZipWriter, abs: &Path, name: &str) -> Result<()> {
    let m = fs::symlink_metadata(abs).map_err(at(abs))?;
    if m.is_dir() {
        z.dir(&format!("{name}/"), &m)?;
        let mut names: Vec<String> = fs::read_dir(abs)
            .map_err(at(abs))?
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        for e in names {
            add_to(z, &abs.join(&e), &format!("{name}/{e}"))?;
        }
        return Ok(());
    }
    let file = if m.file_type().is_symlink() {
        match fs::metadata(abs) {
            Ok(f) => f,
            Err(_) => return Ok(()),
        }
    } else {
        m
    };
    if file.is_file() {
        z.file(name, abs, &file)?;
    }
    Ok(())
}

/// The most a zip without Zip64 holds: sizes and offsets are four bytes.
const ZIP_MAX: u64 = 0xffff_ffff;

struct Entry {
    name: Vec<u8>,
    crc: u32,
    size: u32,
    packed: u32,
    offset: u32,
    time: u16,
    date: u16,
    mode: u32,
    dir: bool,
}

/// Writes a zip file as it goes: each file deflated as it is read, its
/// sizes and checksum in a data descriptor after it, and the central
/// directory at the end.
struct ZipWriter {
    out: io::BufWriter<fs::File>,
    offset: u64,
    entries: Vec<Entry>,
}

impl ZipWriter {
    fn write(&mut self, b: &[u8]) -> Result<()> {
        self.offset += b.len() as u64;
        if self.offset > ZIP_MAX {
            return Err(Error::msg("too large to download as a zip"));
        }
        self.out.write_all(b).map_err(Error::from)
    }

    fn header(name: &[u8], time: u16, date: u16, flags: u16, method: u16) -> Vec<u8> {
        let mut h = vec![0u8; 30];
        h[0..4].copy_from_slice(&0x04034b50u32.to_le_bytes());
        h[4..6].copy_from_slice(&20u16.to_le_bytes());
        h[6..8].copy_from_slice(&flags.to_le_bytes());
        h[8..10].copy_from_slice(&method.to_le_bytes());
        h[10..12].copy_from_slice(&time.to_le_bytes());
        h[12..14].copy_from_slice(&date.to_le_bytes());
        h[26..28].copy_from_slice(&(name.len() as u16).to_le_bytes());
        h.extend_from_slice(name);
        h
    }

    fn dir(&mut self, name: &str, m: &fs::Metadata) -> Result<()> {
        let n = name.as_bytes().to_vec();
        let (time, date) = dos_time(m.mtime());
        let offset = self.offset as u32;
        self.write(&Self::header(&n, time, date, 0x0800, 0))?;
        self.entries.push(Entry {
            name: n,
            crc: 0,
            size: 0,
            packed: 0,
            offset,
            time,
            date,
            mode: m.mode(),
            dir: true,
        });
        Ok(())
    }

    fn file(&mut self, name: &str, abs: &Path, m: &fs::Metadata) -> Result<()> {
        let n = name.as_bytes().to_vec();
        let (time, date) = dos_time(m.mtime());
        let offset = self.offset as u32;
        // Bit 3: the sizes follow the data; bit 11: the name is UTF-8.
        self.write(&Self::header(&n, time, date, 0x0808, 8))?;
        let mut src = fs::File::open(abs).map_err(at(abs))?;
        let mut crc = crc32fast::Hasher::new();
        let mut size: u64 = 0;
        let mut deflate =
            flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
        let mut packed: u64 = 0;
        let mut buf = vec![0u8; 64 << 10];
        loop {
            let got = src.read(&mut buf).map_err(at(abs))?;
            if got == 0 {
                break;
            }
            crc.update(&buf[..got]);
            size += got as u64;
            if size > ZIP_MAX {
                return Err(Error::msg("too large to download as a zip"));
            }
            deflate.write_all(&buf[..got]).map_err(Error::from)?;
            let chunk = std::mem::take(deflate.get_mut());
            packed += chunk.len() as u64;
            self.write(&chunk)?;
        }
        let rest = deflate.finish().map_err(Error::from)?;
        packed += rest.len() as u64;
        self.write(&rest)?;
        let crc = crc.finalize();
        let mut d = Vec::with_capacity(16);
        d.extend_from_slice(&0x08074b50u32.to_le_bytes());
        d.extend_from_slice(&crc.to_le_bytes());
        d.extend_from_slice(&(packed as u32).to_le_bytes());
        d.extend_from_slice(&(size as u32).to_le_bytes());
        self.write(&d)?;
        self.entries.push(Entry {
            name: n,
            crc,
            size: size as u32,
            packed: packed as u32,
            offset,
            time,
            date,
            mode: m.mode(),
            dir: false,
        });
        Ok(())
    }

    fn finish(&mut self) -> Result<()> {
        let start = self.offset;
        let entries = std::mem::take(&mut self.entries);
        for e in &entries {
            let mut c = vec![0u8; 46];
            c[0..4].copy_from_slice(&0x02014b50u32.to_le_bytes());
            // Made by Unix, so the modes below are read.
            c[4..6].copy_from_slice(&((3u16 << 8) | 20).to_le_bytes());
            c[6..8].copy_from_slice(&20u16.to_le_bytes());
            c[8..10].copy_from_slice(&(if e.dir { 0x0800u16 } else { 0x0808 }).to_le_bytes());
            c[10..12].copy_from_slice(&(if e.dir { 0u16 } else { 8 }).to_le_bytes());
            c[12..14].copy_from_slice(&e.time.to_le_bytes());
            c[14..16].copy_from_slice(&e.date.to_le_bytes());
            c[16..20].copy_from_slice(&e.crc.to_le_bytes());
            c[20..24].copy_from_slice(&e.packed.to_le_bytes());
            c[24..28].copy_from_slice(&e.size.to_le_bytes());
            c[28..30].copy_from_slice(&(e.name.len() as u16).to_le_bytes());
            let attr = ((e.mode & 0xffff) << 16) | if e.dir { 0x10 } else { 0 };
            c[38..42].copy_from_slice(&attr.to_le_bytes());
            c[42..46].copy_from_slice(&e.offset.to_le_bytes());
            c.extend_from_slice(&e.name);
            self.write(&c)?;
        }
        let mut end = vec![0u8; 22];
        end[0..4].copy_from_slice(&0x06054b50u32.to_le_bytes());
        let count = entries.len().min(0xffff) as u16;
        end[8..10].copy_from_slice(&count.to_le_bytes());
        end[10..12].copy_from_slice(&count.to_le_bytes());
        end[12..16].copy_from_slice(&((self.offset - start) as u32).to_le_bytes());
        end[16..20].copy_from_slice(&(start as u32).to_le_bytes());
        self.write(&end)?;
        self.out.flush().map_err(Error::from)
    }
}

struct LocalTime {
    year: i32,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
}

fn local_time(t: SystemTime) -> LocalTime {
    let secs = t.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    local_of(secs as i64)
}

fn local_of(secs: i64) -> LocalTime {
    // SAFETY: tm is plain data, which localtime_r fills in. time_t is
    // 64-bit on every target the agent is built for.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe {
        libc::localtime_r(&secs, &mut tm);
    }
    LocalTime {
        year: tm.tm_year + 1900,
        month: (tm.tm_mon + 1) as u32,
        day: tm.tm_mday as u32,
        hour: tm.tm_hour as u32,
        minute: tm.tm_min as u32,
        second: tm.tm_sec as u32,
    }
}

/// A time as a zip has it: two-second time, and date from 1980.
fn dos_time(mtime: i64) -> (u16, u16) {
    let t = local_of(mtime);
    let y = t.year.max(1980) as u32;
    (
        ((t.hour << 11) | (t.minute << 5) | (t.second >> 1)) as u16,
        (((y - 1980) << 9) | (t.month << 5) | t.day) as u16,
    )
}

// --- Permissions ------------------------------------------------------------------

/// Makes a file executable or not -- by whoever may read it, as chmod +x
/// does -- and writable by its owner or by nobody.
pub fn set_mode(path: &Path, exec: Option<bool>, writable: Option<bool>) -> Result<u32> {
    let mut mode = fs::metadata(path).map_err(at(path))?.mode() & 0o7777;
    if let Some(x) = exec {
        mode = if x {
            mode | ((mode & 0o444) >> 2)
        } else {
            mode & !0o111
        };
    }
    if let Some(w) = writable {
        mode = if w { mode | 0o200 } else { mode & !0o222 };
    }
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).map_err(at(path))?;
    Ok(mode)
}

// --- Line separators, encoding and byte order mark --------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Encoding {
    #[serde(rename = "UTF-8")]
    Utf8,
    #[serde(rename = "UTF-16LE")]
    Utf16Le,
    #[serde(rename = "UTF-16BE")]
    Utf16Be,
    #[serde(rename = "ISO-8859-1")]
    Latin1,
}

impl Encoding {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "UTF-8" => Encoding::Utf8,
            "UTF-16LE" => Encoding::Utf16Le,
            "UTF-16BE" => Encoding::Utf16Be,
            "ISO-8859-1" => Encoding::Latin1,
            _ => return None,
        })
    }

    fn mark(self) -> &'static [u8] {
        match self {
            Encoding::Utf8 => &[0xef, 0xbb, 0xbf],
            Encoding::Utf16Le => &[0xff, 0xfe],
            Encoding::Utf16Be => &[0xfe, 0xff],
            Encoding::Latin1 => &[],
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Separator {
    Lf,
    Crlf,
    Cr,
}

impl Separator {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "LF" => Separator::Lf,
            "CRLF" => Separator::Crlf,
            "CR" => Separator::Cr,
            _ => return None,
        })
    }
}

/// What a file's bytes say of its text: how it is encoded, whether it starts
/// with a byte order mark, and how its lines end -- one kind, several
/// ("mixed"), or none (a file of one line). Its mode too.
#[derive(Serialize)]
pub struct TextProps {
    encoding: Option<Encoding>,
    bom: bool,
    separator: Option<&'static str>,
    mode: u32,
    size: u64,
    modified: f64,
}

/// The most of a file read to judge it, and the largest file converted.
const MAX_CONVERT: u64 = 64 << 20;

/// How bytes are encoded: by their byte order mark, else UTF-8 if they are
/// valid UTF-8, else ISO-8859-1, which any bytes are; None for bytes that
/// are not text (a NUL, and no mark saying UTF-16).
fn sniff(b: &[u8]) -> (Option<Encoding>, bool) {
    for e in [Encoding::Utf8, Encoding::Utf16Le, Encoding::Utf16Be] {
        if b.starts_with(e.mark()) {
            return (Some(e), true);
        }
    }
    if b[..b.len().min(8000)].contains(&0) {
        return (None, false);
    }
    if std::str::from_utf8(b).is_ok() {
        (Some(Encoding::Utf8), false)
    } else {
        (Some(Encoding::Latin1), false)
    }
}

fn decode(b: &[u8], e: Encoding, bom: bool) -> String {
    let body = if bom { &b[e.mark().len()..] } else { b };
    match e {
        Encoding::Utf8 => String::from_utf8_lossy(body).into_owned(),
        Encoding::Utf16Le | Encoding::Utf16Be => {
            let units: Vec<u16> = body
                .as_chunks::<2>()
                .0
                .iter()
                .map(|&c| {
                    if e == Encoding::Utf16Le {
                        u16::from_le_bytes(c)
                    } else {
                        u16::from_be_bytes(c)
                    }
                })
                .collect();
            String::from_utf16_lossy(&units)
        }
        Encoding::Latin1 => body.iter().map(|&b| b as char).collect(),
    }
}

fn encode(s: &str, e: Encoding, bom: bool) -> Result<Vec<u8>> {
    let mut out: Vec<u8> = if bom { e.mark().to_vec() } else { Vec::new() };
    match e {
        Encoding::Utf8 => out.extend_from_slice(s.as_bytes()),
        Encoding::Utf16Le => s.encode_utf16().for_each(|u| out.extend(u.to_le_bytes())),
        Encoding::Utf16Be => s.encode_utf16().for_each(|u| out.extend(u.to_be_bytes())),
        Encoding::Latin1 => {
            for c in s.chars() {
                if c as u32 > 0xff {
                    return Err(Error::msg("the file has characters ISO-8859-1 cannot hold"));
                }
                out.push(c as u32 as u8);
            }
        }
    }
    Ok(out)
}

fn separator_of(s: &str) -> Option<&'static str> {
    let crlf = s.matches("\r\n").count();
    let lf = s.matches('\n').count() - crlf;
    let cr = s.matches('\r').count() - crlf;
    let kinds: Vec<&str> = [(lf, "LF"), (crlf, "CRLF"), (cr, "CR")]
        .iter()
        .filter(|(n, _)| *n > 0)
        .map(|(_, k)| *k)
        .collect();
    match kinds.len() {
        0 => None,
        1 => Some(kinds[0]),
        _ => Some("mixed"),
    }
}

pub fn text_props(path: &Path) -> Result<TextProps> {
    let m = fs::metadata(path).map_err(at(path))?;
    if !m.is_file() {
        return Err(Error::msg(format!("{} is not a file", base(path))));
    }
    let mut f = fs::File::open(path).map_err(at(path))?;
    let mut b = Vec::new();
    Read::by_ref(&mut f)
        .take(m.len().min(MAX_CONVERT))
        .read_to_end(&mut b)
        .map_err(at(path))?;
    let (encoding, bom) = sniff(&b);
    let modified = m.mtime() as f64 * 1000.0 + (m.mtime_nsec() as f64 / 1e6);
    Ok(TextProps {
        encoding,
        bom,
        separator: encoding.and_then(|e| separator_of(&decode(&b, e, bom))),
        mode: m.mode() & 0o7777,
        size: m.len(),
        modified,
    })
}

/// Rewrites a text file with other line separators, in another encoding,
/// or with its byte order mark added or taken away. The text is read as it
/// is encoded, so only its bytes change.
pub fn convert(
    path: &Path,
    separator: Option<Separator>,
    encoding: Option<Encoding>,
    bom: Option<bool>,
) -> Result<TextProps> {
    let m = fs::metadata(path).map_err(at(path))?;
    if m.len() > MAX_CONVERT {
        return Err(Error::msg(format!(
            "{} is too large to convert",
            base(path)
        )));
    }
    let b = fs::read(path).map_err(at(path))?;
    let (Some(from), had) = sniff(&b) else {
        return Err(Error::msg(format!("{} is not text", base(path))));
    };
    let mut s = decode(&b, from, had);
    if let Some(sep) = separator {
        let to = match sep {
            Separator::Lf => "\n",
            Separator::Crlf => "\r\n",
            Separator::Cr => "\r",
        };
        s = s.replace("\r\n", "\n").replace('\r', "\n");
        if to != "\n" {
            s = s.replace('\n', to);
        }
    }
    let enc = encoding.unwrap_or(from);
    // UTF-16 is marked: without a mark, nothing tells its byte order.
    let mark = bom.unwrap_or(match encoding {
        Some(e) if e != from => matches!(enc, Encoding::Utf16Le | Encoding::Utf16Be),
        _ => had,
    });
    if mark && enc == Encoding::Latin1 {
        return Err(Error::msg("ISO-8859-1 has no byte order mark"));
    }
    fs::write(path, encode(&s, enc, mark)?).map_err(at(path))?;
    text_props(path)
}
