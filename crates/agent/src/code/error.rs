use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

/// What went wrong, as a person reads it: a file system error said in
/// words with the path it is about ("/workspace/x does not exist"), anything
/// else as its own message.
#[derive(Debug)]
pub enum Error {
    Io {
        err: io::Error,
        path: Option<PathBuf>,
        dest: Option<PathBuf>,
    },
    Msg(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn msg(m: impl Into<String>) -> Self {
        Error::Msg(m.into())
    }
}

/// Wraps an I/O error with the path it was about.
pub fn at(path: &Path) -> impl FnOnce(io::Error) -> Error + '_ {
    move |err| Error::Io {
        err,
        path: Some(path.to_path_buf()),
        dest: None,
    }
}

/// Wraps an I/O error from an operation on two paths, a rename or a copy.
pub fn between<'a>(from: &'a Path, to: &'a Path) -> impl FnOnce(io::Error) -> Error + 'a {
    move |err| Error::Io {
        err,
        path: Some(from.to_path_buf()),
        dest: Some(to.to_path_buf()),
    }
}

impl From<io::Error> for Error {
    fn from(err: io::Error) -> Self {
        Error::Io {
            err,
            path: None,
            dest: None,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (err, path, dest) = match self {
            Error::Msg(m) => return f.write_str(m),
            Error::Io { err, path, dest } => (err, path, dest),
        };
        let path = path
            .as_deref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "The file".into());
        let dest = dest
            .as_deref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| path.clone());
        match err.raw_os_error() {
            Some(libc::ENOENT) => write!(f, "{path} does not exist"),
            Some(libc::EACCES | libc::EPERM) => write!(f, "{path}: permission denied"),
            Some(libc::ENOTDIR) => write!(f, "{path} is not a folder"),
            Some(libc::EISDIR) => write!(f, "{path} is a folder"),
            Some(libc::EEXIST) => write!(f, "{dest} already exists"),
            Some(libc::ENOTEMPTY) => write!(f, "{path} is not empty"),
            Some(libc::ENOSPC) => f.write_str("The disk is full"),
            Some(libc::EDQUOT) => f.write_str("The disk quota is used up"),
            Some(libc::EROFS) => write!(f, "{path} is on a read-only file system"),
            Some(libc::EBUSY) => write!(f, "{path} is in use"),
            Some(libc::ELOOP) => write!(f, "{path} is a link that leads back to itself"),
            Some(libc::ENAMETOOLONG) => write!(f, "{path} is too long a name"),
            Some(libc::EMFILE | libc::ENFILE) => f.write_str("Too many files are open"),
            Some(libc::EXDEV) => write!(f, "{dest} is on another file system"),
            _ => write!(f, "{err}"),
        }
    }
}
