//! Reaching the shared files: the worker serves each copy over NFS, on
//! vsock, and hangarfs routes the shared paths there.

use std::io;
use std::path::{Path, PathBuf};

use super::routes::Route;
use crate::user::User;

/// How the guest reaches the shared files.
pub trait Files: Send + Sync + 'static {
    /// Makes `shared` the copies' files, if it is not yet.
    fn mount(&self) -> io::Result<()>;
    /// Has the routes applied: each route's path served from its target
    /// under `shared`.
    fn route(&self, routes: &[Route]) -> io::Result<()>;
    /// Where the copies are: `<shared>/<copy>/<path in copy>`.
    fn shared(&self) -> &Path;
    /// A directory on the environment's own disk, kept across restarts: the
    /// routes last applied, and the files kept aside in conflicts.
    fn state(&self) -> &Path;
}

/// Where the files the environment shares with others are mounted, each
/// copy a directory named by its ID.
pub const SHARED: &str = "/run/hangar/files";

/// Where the agent keeps, on the environment's own disk, what it routed
/// and the files it kept aside in conflicts.
pub const STATE: &str = "/var/lib/hangar-agent";

/// The guest's own: the worker's NFS server, and hangarfs.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub struct Router {
    user: User,
    shared: PathBuf,
    state: PathBuf,
    mounted: std::sync::Mutex<bool>,
}

impl Router {
    /// Routes the shared files for `user`, who owns what it makes in them.
    pub fn new(user: User) -> Self {
        Self {
            user,
            shared: SHARED.into(),
            state: STATE.into(),
            mounted: std::sync::Mutex::new(false),
        }
    }
}

impl Files for Router {
    #[cfg(target_os = "linux")]
    fn mount(&self) -> io::Result<()> {
        linux::mount(self)
    }

    #[cfg(not(target_os = "linux"))]
    fn mount(&self) -> io::Result<()> {
        Err(io::Error::other(
            "the shared files are only mounted in a Linux guest",
        ))
    }

    #[cfg(target_os = "linux")]
    fn route(&self, routes: &[Route]) -> io::Result<()> {
        linux::route(self, routes)
    }

    #[cfg(not(target_os = "linux"))]
    fn route(&self, _: &[Route]) -> io::Result<()> {
        Err(io::Error::other(
            "the shared files are only routed in a Linux guest",
        ))
    }

    fn shared(&self) -> &Path {
        &self.shared
    }

    fn state(&self) -> &Path {
        &self.state
    }
}

/// Runs `f` as `user` in the filesystem's eyes, as an NFS server does a
/// client's: on a thread of its own whose filesystem uid and gid are the
/// user's, with their own group, so the kernel checks permissions and gives
/// new files their owner as it would for them. A process not running as
/// root cannot, and runs `f` as itself.
pub fn as_user<R: Send>(user: &User, f: impl FnOnce() -> R + Send) -> R {
    #[cfg(target_os = "linux")]
    // SAFETY: geteuid cannot fail.
    if unsafe { libc::geteuid() } == 0 {
        let (uid, gid) = (user.uid, user.gid);
        return std::thread::scope(|s| {
            s.spawn(move || {
                // The thread's own groups, through the raw call: the C
                // library's changes every thread's. setfsuid and setfsgid
                // are the thread's alone.
                // SAFETY: each call changes only this thread's credentials,
                // and the groups are read from a live array.
                unsafe {
                    let groups = [gid];
                    libc::syscall(libc::SYS_setgroups, 1, groups.as_ptr());
                    libc::setfsgid(gid);
                    libc::setfsuid(uid);
                }
                f()
            })
            .join()
            .unwrap_or_else(|p| std::panic::resume_unwind(p))
        });
    }
    let _ = user;
    f()
}

#[cfg(target_os = "linux")]
mod linux {
    use std::ffi::CString;
    use std::fs::File;
    use std::io;
    use std::os::fd::AsRawFd;

    use super::{Route, Router, as_user};
    use crate::profile::moves::mkdir_all_owned;

    /// The vsock port the worker serves the shared files on, from the host.
    const PORT: u32 = 2049;
    const HOST_CID: u32 = 2;

    /// hangarfs's routes (include/uapi/linux/hangarfs.h).
    #[repr(C)]
    struct Routes {
        shared_fd: i32,
        len: u32,
        buf: u64,
    }

    const ROUTE_FILE: u8 = b'f';
    const ROUTE_DIR: u8 = b'd';
    const ROUTE_EXCLUDE: u8 = b'x';
    /// _IOW('H', 1, struct hangarfs_routes)
    const IOC_ROUTES: u32 =
        1 << 30 | (std::mem::size_of::<Routes>() as u32) << 16 | (b'H' as u32) << 8 | 1;

    pub(super) fn mount(r: &Router) -> io::Result<()> {
        let mut mounted = r.mounted.lock().unwrap();
        if *mounted {
            return Ok(());
        }
        std::fs::create_dir_all(&r.shared)?;
        // Only names that are there are cached, and what is known of a file
        // or directory for a second: a file another environment makes is
        // seen at once, and one it removes -- a lock directory let go of --
        // within a second.
        let options = CString::new(format!(
            "vers=4.2,proto=vsock,addr=vsock:{HOST_CID},port={PORT},hard,lookupcache=positive,actimeo=1"
        ))?;
        let target = CString::new(r.shared.as_os_str().as_encoded_bytes())?;
        // SAFETY: every pointer is to a live, NUL-terminated string.
        let rc = unsafe {
            libc::mount(
                c"hangar:/".as_ptr(),
                target.as_ptr(),
                c"nfs4".as_ptr(),
                libc::MS_NOSUID | libc::MS_NODEV,
                options.as_ptr().cast(),
            )
        };
        if rc < 0 {
            let e = io::Error::last_os_error();
            return Err(io::Error::new(
                e.kind(),
                format!("mounting the shared files: {e}"),
            ));
        }
        *mounted = true;
        Ok(())
    }

    /// Has hangarfs serve each route's path from its target, making the
    /// directories a target is in -- and a directory's target -- if they
    /// are not there: a name whose target's directory is missing cannot be
    /// made.
    pub(super) fn route(r: &Router, routes: &[Route]) -> io::Result<()> {
        mount(r)?;
        let mut buf = vec![];
        for rt in routes {
            let (kind, dir) = if rt.exclude {
                (ROUTE_EXCLUDE, None)
            } else if rt.dir {
                (ROUTE_DIR, Some(rt.target.as_str()))
            } else {
                (
                    ROUTE_FILE,
                    Some(rt.target.rsplit_once('/').map_or("", |(d, _)| d)),
                )
            };
            if let Some(dir) = dir {
                let dir = r.shared.join(dir);
                as_user(&r.user, || mkdir_all_owned(&dir, None)).map_err(|e| {
                    io::Error::new(
                        e.kind(),
                        format!("making {} in the shared files: {e}", dir.display()),
                    )
                })?;
            }
            buf.push(kind);
            buf.extend_from_slice(rt.path.as_bytes());
            buf.push(0);
            buf.extend_from_slice(rt.target.as_bytes());
            buf.push(0);
        }
        let shared = File::open(&r.shared)?;
        let root = File::open("/")?;
        let mut arg = Routes {
            shared_fd: shared.as_raw_fd(),
            len: buf.len() as u32,
            buf: if buf.is_empty() {
                0
            } else {
                buf.as_ptr() as u64
            },
        };
        // SAFETY: the argument and the buffer it points to outlive the call.
        if unsafe { libc::ioctl(root.as_raw_fd(), IOC_ROUTES as _, &raw mut arg) } < 0 {
            let e = io::Error::last_os_error();
            return Err(io::Error::new(
                e.kind(),
                format!("setting hangarfs's routes: {e}"),
            ));
        }
        Ok(())
    }
}
