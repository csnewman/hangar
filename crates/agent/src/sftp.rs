//! `hangar-agent sftp-server`: SFTP on standard input and output, as
//! OpenSSH's sftp-server serves it, with the permissions of whoever runs
//! it. The SSH server runs it, as the user, for an image with no OpenSSH
//! one.
//!
//! The protocol is version 3, as OpenSSH speaks it, with its
//! posix-rename@openssh.com. Relative paths are from the home directory.

use std::collections::HashMap;
use std::ffi::CString;
use std::fs::{self, File, Metadata};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{
    DirBuilderExt, FileExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt,
};
use std::path::{Component, Path, PathBuf};

use russh_sftp::protocol::{
    Attrs, Data, File as Entry, FileAttributes, Handle, Name, OpenFlags, Packet, Status,
    StatusCode, Version,
};
use russh_sftp::server::{Handler, StatusReply};
use tokio::io::{AsyncRead, AsyncWrite};

/// Serves SFTP on standard input and output until the client goes.
pub async fn serve() -> io::Result<()> {
    if let Some(home) = std::env::var_os("HOME") {
        let _ = std::env::set_current_dir(home);
    }
    let (ended, done) = tokio::sync::oneshot::channel::<()>();
    let io = Stdio {
        stdin: tokio::io::stdin(),
        stdout: tokio::io::stdout(),
        _ended: ended,
    };
    russh_sftp::server::run(io, Server::default()).await;
    // The server holds the stream until the client has gone.
    let _ = done.await;
    Ok(())
}

/// Standard input and output as one stream, which says when it is dropped.
struct Stdio {
    stdin: tokio::io::Stdin,
    stdout: tokio::io::Stdout,
    _ended: tokio::sync::oneshot::Sender<()>,
}

impl AsyncRead for Stdio {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        std::pin::Pin::new(&mut self.stdin).poll_read(cx, buf)
    }
}

impl AsyncWrite for Stdio {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<io::Result<usize>> {
        std::pin::Pin::new(&mut self.stdout).poll_write(cx, buf)
    }

    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        std::pin::Pin::new(&mut self.stdout).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        std::pin::Pin::new(&mut self.stdout).poll_shutdown(cx)
    }
}

const POSIX_RENAME: &str = "posix-rename@openssh.com";

/// The most a read gives at once, as OpenSSH's.
const MAX_READ: u32 = 256 << 10;

/// How many names a directory read gives at once.
const DIR_BATCH: usize = 100;

#[derive(Default)]
pub struct Server {
    handles: HashMap<String, Open>,
    next: u64,
    /// Names of users and groups, as listings show them.
    users: HashMap<u32, String>,
    groups: HashMap<u32, String>,
}

enum Open {
    File {
        file: File,
        append: bool,
    },
    /// A directory's names not yet read.
    Dir(Vec<Entry>),
}

fn failed(e: io::Error) -> StatusReply {
    let code = match e.kind() {
        io::ErrorKind::NotFound => StatusCode::NoSuchFile,
        io::ErrorKind::PermissionDenied => StatusCode::PermissionDenied,
        _ => StatusCode::Failure,
    };
    code.with_message(e.to_string())
}

fn ok(id: u32) -> Status {
    Status {
        id,
        status_code: StatusCode::Ok,
        error_message: "Ok".into(),
        language_tag: "en-US".into(),
    }
}

/// A file's attributes as SFTP has them: its whole mode, type and all.
fn attrs(md: &Metadata) -> FileAttributes {
    FileAttributes {
        size: Some(md.size()),
        uid: Some(md.uid()),
        gid: Some(md.gid()),
        permissions: Some(md.mode()),
        atime: Some(md.atime().clamp(0, u32::MAX as i64) as u32),
        mtime: Some(md.mtime().clamp(0, u32::MAX as i64) as u32),
        ..FileAttributes::default()
    }
}

fn times(a: &FileAttributes) -> Option<[libc::timespec; 2]> {
    let t = |s: u32| libc::timespec {
        tv_sec: s.into(),
        tv_nsec: 0,
    };
    Some([t(a.atime?), t(a.mtime?)])
}

fn check(rc: libc::c_int) -> io::Result<()> {
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Sets what of `a` is given on the file at `path`: its size, mode, owner
/// and times.
fn set_attrs(path: &Path, a: &FileAttributes) -> io::Result<()> {
    if let Some(size) = a.size {
        fs::OpenOptions::new()
            .write(true)
            .open(path)?
            .set_len(size)?;
    }
    if let Some(mode) = a.permissions {
        fs::set_permissions(path, fs::Permissions::from_mode(mode & 0o7777))?;
    }
    if a.uid.is_some() || a.gid.is_some() {
        std::os::unix::fs::chown(path, a.uid, a.gid)?;
    }
    if let Some(t) = times(a) {
        let c = CString::new(path.as_os_str().as_bytes())?;
        // SAFETY: a NUL-terminated path and two timespecs.
        check(unsafe { libc::utimensat(libc::AT_FDCWD, c.as_ptr(), t.as_ptr(), 0) })?;
    }
    Ok(())
}

/// Sets what of `a` is given on an open file.
fn set_file_attrs(file: &File, a: &FileAttributes) -> io::Result<()> {
    let fd = std::os::fd::AsRawFd::as_raw_fd(file);
    if let Some(size) = a.size {
        file.set_len(size)?;
    }
    // SAFETY: each call is on a descriptor this server holds open.
    unsafe {
        if let Some(mode) = a.permissions {
            check(libc::fchmod(fd, (mode & 0o7777) as libc::mode_t))?;
        }
        if a.uid.is_some() || a.gid.is_some() {
            // -1 leaves an ID as it is.
            check(libc::fchown(
                fd,
                a.uid.unwrap_or(u32::MAX),
                a.gid.unwrap_or(u32::MAX),
            ))?;
        }
        if let Some(t) = times(a) {
            check(libc::futimens(fd, t.as_ptr()))?;
        }
    }
    Ok(())
}

/// `p` made absolute from the working directory, and cleaned, without
/// following links.
fn absolute(p: &str) -> PathBuf {
    let start = if p.starts_with('/') {
        PathBuf::from("/")
    } else {
        std::env::current_dir().unwrap_or_else(|_| "/".into())
    };
    let mut out = start;
    for c in Path::new(p).components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(n) => out.push(n),
            _ => {}
        }
    }
    out
}

fn name(id: u32, filename: impl Into<String>) -> Name {
    Name {
        id,
        files: vec![Entry::dummy(filename)],
    }
}

impl Server {
    /// A directory listing's line for a file, as OpenSSH's sftp-server
    /// writes it, which is `ls -l`'s.
    fn longname(&mut self, name: &str, md: &Metadata) -> String {
        let mode = md.mode();
        let t = md.file_type();
        let kind = if t.is_dir() {
            'd'
        } else if t.is_symlink() {
            'l'
        } else if t.is_char_device() {
            'c'
        } else if t.is_block_device() {
            'b'
        } else if t.is_fifo() {
            'p'
        } else if t.is_socket() {
            's'
        } else {
            '-'
        };
        let mut perms = String::from(kind);
        for (i, c) in "rwxrwxrwx".chars().enumerate() {
            perms.push(if mode & (0o400 >> i) != 0 { c } else { '-' });
        }
        let user = self
            .users
            .entry(md.uid())
            .or_insert_with(|| {
                crate::user::User::name_of(md.uid()).unwrap_or_else(|| md.uid().to_string())
            })
            .clone();
        let group = self
            .groups
            .entry(md.gid())
            .or_insert_with(|| group_name(md.gid()))
            .clone();
        format!(
            "{perms} {:>3} {user:<8} {group:<8} {:>8} {} {name}",
            md.nlink(),
            md.size(),
            listing_date(md.mtime())
        )
    }

    fn handle(&mut self, open: Open) -> String {
        self.next += 1;
        let h = self.next.to_string();
        self.handles.insert(h.clone(), open);
        h
    }

    fn file(&self, handle: &str) -> Result<(&File, bool), StatusReply> {
        match self.handles.get(handle) {
            Some(Open::File { file, append }) => Ok((file, *append)),
            _ => Err(StatusCode::Failure.with_message("no such handle")),
        }
    }
}

/// The name of group `gid`, from /etc/group, else its number.
fn group_name(gid: u32) -> String {
    let id = gid.to_string();
    fs::read_to_string("/etc/group")
        .ok()
        .and_then(|g| {
            g.lines()
                .map(|l| l.split(':').collect::<Vec<_>>())
                .find(|f| f.len() >= 3 && f[2] == id)
                .map(|f| f[0].to_string())
        })
        .unwrap_or(id)
}

/// A modification time as `ls -l` shows one: the time of day within six
/// months, else the year.
fn listing_date(mtime: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    let recent = (now - mtime).abs() < 182 * 24 * 3600;
    let format = if recent { c"%b %e %H:%M" } else { c"%b %e  %Y" };
    let t = mtime;
    let mut buf = [0u8; 64];
    // SAFETY: tm is plain data, which localtime_r fills in; strftime writes
    // at most the buffer's length.
    let n = unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&t, &mut tm).is_null() {
            return String::new();
        }
        libc::strftime(buf.as_mut_ptr().cast(), buf.len(), format.as_ptr(), &tm)
    };
    String::from_utf8_lossy(&buf[..n]).into_owned()
}

/// The two strings of an extension's request.
fn two_strings(data: &[u8]) -> Option<(String, String)> {
    let mut r = data;
    let mut string = || {
        let (n, rest) = r.split_first_chunk::<4>()?;
        let n = u32::from_be_bytes(*n) as usize;
        if rest.len() < n {
            return None;
        }
        let (s, rest) = rest.split_at(n);
        r = rest;
        Some(String::from_utf8_lossy(s).into_owned())
    };
    Some((string()?, string()?))
}

impl Handler for Server {
    type Error = StatusReply;

    fn unimplemented(&self) -> Self::Error {
        StatusCode::OpUnsupported.into()
    }

    async fn init(
        &mut self,
        _version: u32,
        _extensions: HashMap<String, String>,
    ) -> Result<Version, Self::Error> {
        let mut v = Version::new();
        v.extensions.insert(POSIX_RENAME.into(), "1".into());
        Ok(v)
    }

    async fn open(
        &mut self,
        id: u32,
        filename: String,
        pflags: OpenFlags,
        attrs: FileAttributes,
    ) -> Result<Handle, Self::Error> {
        let mut o = fs::OpenOptions::new();
        let append = pflags.contains(OpenFlags::APPEND);
        o.read(pflags.contains(OpenFlags::READ))
            .write(pflags.contains(OpenFlags::WRITE) || append)
            .append(append)
            .truncate(pflags.contains(OpenFlags::TRUNCATE))
            .mode(attrs.permissions.map_or(0o666, |m| m & 0o7777));
        if pflags.contains(OpenFlags::CREATE) {
            if pflags.contains(OpenFlags::EXCLUDE) {
                o.create_new(true);
            } else {
                o.create(true);
            }
        }
        let file = o.open(&filename).map_err(failed)?;
        Ok(Handle {
            id,
            handle: self.handle(Open::File { file, append }),
        })
    }

    async fn close(&mut self, id: u32, handle: String) -> Result<Status, Self::Error> {
        match self.handles.remove(&handle) {
            Some(_) => Ok(ok(id)),
            None => Err(StatusCode::Failure.with_message("no such handle")),
        }
    }

    async fn read(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        len: u32,
    ) -> Result<Data, Self::Error> {
        let (file, _) = self.file(&handle)?;
        let mut data = vec![0; len.min(MAX_READ) as usize];
        let n = file.read_at(&mut data, offset).map_err(failed)?;
        if n == 0 && !data.is_empty() {
            return Err(StatusCode::Eof.into());
        }
        data.truncate(n);
        Ok(Data { id, data })
    }

    async fn write(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        data: Vec<u8>,
    ) -> Result<Status, Self::Error> {
        let (mut file, append) = self.file(&handle)?;
        if append {
            io::Write::write_all(&mut file, &data).map_err(failed)?;
        } else {
            file.write_all_at(&data, offset).map_err(failed)?;
        }
        Ok(ok(id))
    }

    async fn lstat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
        let md = fs::symlink_metadata(&path).map_err(failed)?;
        Ok(Attrs {
            id,
            attrs: attrs(&md),
        })
    }

    async fn stat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
        let md = fs::metadata(&path).map_err(failed)?;
        Ok(Attrs {
            id,
            attrs: attrs(&md),
        })
    }

    async fn fstat(&mut self, id: u32, handle: String) -> Result<Attrs, Self::Error> {
        let (file, _) = self.file(&handle)?;
        let md = file.metadata().map_err(failed)?;
        Ok(Attrs {
            id,
            attrs: attrs(&md),
        })
    }

    async fn setstat(
        &mut self,
        id: u32,
        path: String,
        a: FileAttributes,
    ) -> Result<Status, Self::Error> {
        set_attrs(Path::new(&path), &a).map_err(failed)?;
        Ok(ok(id))
    }

    async fn fsetstat(
        &mut self,
        id: u32,
        handle: String,
        a: FileAttributes,
    ) -> Result<Status, Self::Error> {
        let (file, _) = self.file(&handle)?;
        set_file_attrs(file, &a).map_err(failed)?;
        Ok(ok(id))
    }

    async fn opendir(&mut self, id: u32, path: String) -> Result<Handle, Self::Error> {
        let mut names = vec![];
        for e in fs::read_dir(&path).map_err(failed)? {
            let e = e.map_err(failed)?;
            let Ok(md) = fs::symlink_metadata(e.path()) else {
                continue;
            };
            let filename = e.file_name().to_string_lossy().into_owned();
            names.push(Entry {
                longname: self.longname(&filename, &md),
                filename,
                attrs: attrs(&md),
            });
        }
        names.reverse();
        Ok(Handle {
            id,
            handle: self.handle(Open::Dir(names)),
        })
    }

    async fn readdir(&mut self, id: u32, handle: String) -> Result<Name, Self::Error> {
        let Some(Open::Dir(names)) = self.handles.get_mut(&handle) else {
            return Err(StatusCode::Failure.with_message("no such handle"));
        };
        if names.is_empty() {
            return Err(StatusCode::Eof.into());
        }
        let at = names.len().saturating_sub(DIR_BATCH);
        let mut files = names.split_off(at);
        files.reverse();
        Ok(Name { id, files })
    }

    async fn remove(&mut self, id: u32, filename: String) -> Result<Status, Self::Error> {
        fs::remove_file(filename).map_err(failed)?;
        Ok(ok(id))
    }

    async fn mkdir(
        &mut self,
        id: u32,
        path: String,
        a: FileAttributes,
    ) -> Result<Status, Self::Error> {
        fs::DirBuilder::new()
            .mode(a.permissions.map_or(0o777, |m| m & 0o7777))
            .create(path)
            .map_err(failed)?;
        Ok(ok(id))
    }

    async fn rmdir(&mut self, id: u32, path: String) -> Result<Status, Self::Error> {
        fs::remove_dir(path).map_err(failed)?;
        Ok(ok(id))
    }

    async fn realpath(&mut self, id: u32, path: String) -> Result<Name, Self::Error> {
        let p = if path.is_empty() { "." } else { &path };
        Ok(name(id, absolute(p).to_string_lossy()))
    }

    /// As version 3 has it, a rename does not replace what is there;
    /// posix-rename does.
    async fn rename(
        &mut self,
        id: u32,
        oldpath: String,
        newpath: String,
    ) -> Result<Status, Self::Error> {
        if fs::symlink_metadata(&newpath).is_ok() {
            return Err(StatusCode::Failure.with_message(format!("{newpath} exists")));
        }
        fs::rename(oldpath, newpath).map_err(failed)?;
        Ok(ok(id))
    }

    async fn readlink(&mut self, id: u32, path: String) -> Result<Name, Self::Error> {
        let target = fs::read_link(path).map_err(failed)?;
        Ok(name(id, target.to_string_lossy()))
    }

    /// OpenSSH's clients send the target first and the link second, the
    /// other way round from the draft, and every server follows them.
    async fn symlink(
        &mut self,
        id: u32,
        target: String,
        link: String,
    ) -> Result<Status, Self::Error> {
        std::os::unix::fs::symlink(target, link).map_err(failed)?;
        Ok(ok(id))
    }

    async fn extended(
        &mut self,
        id: u32,
        request: String,
        data: Vec<u8>,
    ) -> Result<Packet, Self::Error> {
        if request != POSIX_RENAME {
            return Err(StatusCode::OpUnsupported.into());
        }
        let (old, new) = two_strings(&data).ok_or(StatusCode::BadMessage)?;
        fs::rename(old, new).map_err(failed)?;
        Ok(Packet::Status(ok(id)))
    }
}
