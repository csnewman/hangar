//! The FUSE options this server negotiates, on top of what the passthrough
//! filesystem asks for.
//!
//! `fuse-backend-rs` replies to FUSE `INIT` with exactly the options its
//! filesystem returns, and its passthrough filesystem returns only the ones
//! whose behaviour it implements itself. Several options are properties of
//! the *transport* rather than the filesystem -- how large a request may be,
//! whether several may be in flight -- and nothing asks for those, so the
//! defaults stand.
//!
//! The default that matters is `max_write`. Without `BIG_WRITES` the guest is
//! told the largest request it may send is one page, so reading a megabyte
//! costs 256 round trips instead of one. virtiofsd adds the same set in its
//! own server; this is where it goes when the server is a library.
//!
//! Everything else here is delegation.

use std::ffi::CStr;
use std::io;
use std::sync::Arc;
use std::time::Duration;

use fuse_backend_rs::abi::fuse_abi::{
    stat64, statvfs64, CreateIn, FsOptions, OpenOptions, SetattrValid,
};
use fuse_backend_rs::abi::virtio_fs::RemovemappingOne;
use fuse_backend_rs::api::filesystem::{
    Context, DirEntry, Entry, FileLock, FileSystem, GetxattrReply, IoctlData, ListxattrReply,
    ZeroCopyReader, ZeroCopyWriter,
};
use fuse_backend_rs::passthrough::PassthroughFs;
use fuse_backend_rs::transport::FsCacheReqHandler;

use crate::state::{Mapping, Session};

/// Options added to whatever the filesystem asks for.
///
/// Each is honoured by the transport in `fs.rs` rather than by the
/// filesystem: the requests arrive whole in a descriptor chain, so a larger
/// request is simply a longer chain, and several queues are already served
/// concurrently.
fn transport_options() -> FsOptions {
    FsOptions::BIG_WRITES
        | FsOptions::MAX_PAGES
        | FsOptions::ASYNC_READ
        | FsOptions::ASYNC_DIO
        | FsOptions::PARALLEL_DIROPS
        | FsOptions::ATOMIC_O_TRUNC
        | FsOptions::AUTO_INVAL_DATA
        | FsOptions::SUBMOUNTS
        | FsOptions::INIT_EXT
}

/// A passthrough filesystem that negotiates the transport's options too, and
/// records what the guest is holding.
///
/// The record is what makes the session outlive this process: see `state`.
pub struct Fs(PassthroughFs, Arc<Session>);

impl Fs {
    pub fn new(inner: PassthroughFs, session: Arc<Session>) -> Self {
        Self(inner, session)
    }

    /// Looks a path up so the filesystem holds its nodeid again.
    ///
    /// Each component is looked up in turn, because a lookup names a parent
    /// nodeid rather than a path. The nodeid that comes back is the one the
    /// guest already has, since it is derived from the file on disk.
    pub fn relookup(&self, ctx: &Context, path: &std::path::Path) -> io::Result<u64> {
        let parts = Session::components(path)
            .ok_or_else(|| io::Error::other("a path that cannot be looked up"))?;
        let mut inode = crate::state::ROOT_ID;
        for part in parts {
            inode = self.0.lookup(ctx, inode, &part)?.inode;
        }
        Ok(inode)
    }

    /// Makes a mapping the guest already believes in.
    pub fn remap(
        &self,
        ctx: &Context,
        m: &Mapping,
        vu_req: &mut dyn FsCacheReqHandler,
    ) -> io::Result<()> {
        self.0
            .setupmapping(ctx, m.inode, 0, m.foffset, m.len, m.flags, m.moffset, vu_req)
    }
}

/// Forward a method to the wrapped filesystem unchanged.
macro_rules! forward {
    ($( fn $name:ident ( &self $(, $arg:ident : $ty:ty )* $(,)? ) $( -> $ret:ty )? ; )*) => {
        $(
            fn $name(&self $(, $arg: $ty)*) $( -> $ret )? {
                self.0.$name($($arg),*)
            }
        )*
    };
}

impl FileSystem for Fs {
    type Inode = <PassthroughFs as FileSystem>::Inode;
    type Handle = <PassthroughFs as FileSystem>::Handle;

    fn lookup(&self, ctx: &Context, parent: Self::Inode, name: &CStr) -> io::Result<Entry> {
        let entry = self.0.lookup(ctx, parent, name)?;
        self.1.lookup(parent, name, entry.inode);
        Ok(entry)
    }

    fn setupmapping(
        &self,
        ctx: &Context,
        inode: Self::Inode,
        handle: Self::Handle,
        foffset: u64,
        len: u64,
        flags: u64,
        moffset: u64,
        vu_req: &mut dyn FsCacheReqHandler,
    ) -> io::Result<()> {
        self.0
            .setupmapping(ctx, inode, handle, foffset, len, flags, moffset, vu_req)?;
        self.1.map(Mapping {
            inode,
            foffset,
            len,
            flags,
            moffset,
        });
        Ok(())
    }

    fn removemapping(
        &self,
        ctx: &Context,
        inode: Self::Inode,
        requests: Vec<RemovemappingOne>,
        vu_req: &mut dyn FsCacheReqHandler,
    ) -> io::Result<()> {
        for r in &requests {
            self.1.unmap(r.moffset, r.len);
        }
        self.0.removemapping(ctx, inode, requests, vu_req)
    }

    fn init(&self, capable: FsOptions) -> io::Result<FsOptions> {
        // `capable` is what the guest offered. Asking for anything outside it
        // would be dropped, so the union is intersected before it is
        // returned.
        Ok((self.0.init(capable)? | transport_options()) & capable)
    }

    forward! {
        fn destroy(&self);
        fn forget(&self, ctx: &Context, inode: Self::Inode, count: u64);
        fn batch_forget(&self, ctx: &Context, requests: Vec<(Self::Inode, u64)>);
        fn getattr(&self, ctx: &Context, inode: Self::Inode, handle: Option<Self::Handle>) -> io::Result<(stat64, Duration)>;
        fn setattr(&self, ctx: &Context, inode: Self::Inode, attr: stat64, handle: Option<Self::Handle>, valid: SetattrValid) -> io::Result<(stat64, Duration)>;
        fn readlink(&self, ctx: &Context, inode: Self::Inode) -> io::Result<Vec<u8>>;
        fn symlink(&self, ctx: &Context, linkname: &CStr, parent: Self::Inode, name: &CStr) -> io::Result<Entry>;
        fn mknod(&self, ctx: &Context, inode: Self::Inode, name: &CStr, mode: u32, rdev: u32, umask: u32) -> io::Result<Entry>;
        fn mkdir(&self, ctx: &Context, parent: Self::Inode, name: &CStr, mode: u32, umask: u32) -> io::Result<Entry>;
        fn unlink(&self, ctx: &Context, parent: Self::Inode, name: &CStr) -> io::Result<()>;
        fn rmdir(&self, ctx: &Context, parent: Self::Inode, name: &CStr) -> io::Result<()>;
        fn rename(&self, ctx: &Context, olddir: Self::Inode, oldname: &CStr, newdir: Self::Inode, newname: &CStr, flags: u32) -> io::Result<()>;
        fn link(&self, ctx: &Context, inode: Self::Inode, newparent: Self::Inode, newname: &CStr) -> io::Result<Entry>;
        fn open(&self, ctx: &Context, inode: Self::Inode, flags: u32, fuse_flags: u32) -> io::Result<(Option<Self::Handle>, OpenOptions, Option<u32>)>;
        fn create(&self, ctx: &Context, parent: Self::Inode, name: &CStr, args: CreateIn) -> io::Result<(Entry, Option<Self::Handle>, OpenOptions, Option<u32>)>;
        fn read(&self, ctx: &Context, inode: Self::Inode, handle: Self::Handle, w: &mut dyn ZeroCopyWriter, size: u32, offset: u64, lock_owner: Option<u64>, flags: u32) -> io::Result<usize>;
        fn write(&self, ctx: &Context, inode: Self::Inode, handle: Self::Handle, r: &mut dyn ZeroCopyReader, size: u32, offset: u64, lock_owner: Option<u64>, delayed_write: bool, flags: u32, fuse_flags: u32) -> io::Result<usize>;
        fn flush(&self, ctx: &Context, inode: Self::Inode, handle: Self::Handle, lock_owner: u64) -> io::Result<()>;
        fn fsync(&self, ctx: &Context, inode: Self::Inode, datasync: bool, handle: Self::Handle) -> io::Result<()>;
        fn fallocate(&self, ctx: &Context, inode: Self::Inode, handle: Self::Handle, mode: u32, offset: u64, length: u64) -> io::Result<()>;
        fn release(&self, ctx: &Context, inode: Self::Inode, flags: u32, handle: Self::Handle, flush: bool, flock_release: bool, lock_owner: Option<u64>) -> io::Result<()>;
        fn statfs(&self, ctx: &Context, inode: Self::Inode) -> io::Result<statvfs64>;
        fn setxattr(&self, ctx: &Context, inode: Self::Inode, name: &CStr, value: &[u8], flags: u32) -> io::Result<()>;
        fn getxattr(&self, ctx: &Context, inode: Self::Inode, name: &CStr, size: u32) -> io::Result<GetxattrReply>;
        fn listxattr(&self, ctx: &Context, inode: Self::Inode, size: u32) -> io::Result<ListxattrReply>;
        fn removexattr(&self, ctx: &Context, inode: Self::Inode, name: &CStr) -> io::Result<()>;
        fn opendir(&self, ctx: &Context, inode: Self::Inode, flags: u32) -> io::Result<(Option<Self::Handle>, OpenOptions)>;
        fn readdir(&self, ctx: &Context, inode: Self::Inode, handle: Self::Handle, size: u32, offset: u64, add_entry: &mut dyn FnMut(DirEntry) -> io::Result<usize>) -> io::Result<()>;
        fn readdirplus(&self, ctx: &Context, inode: Self::Inode, handle: Self::Handle, size: u32, offset: u64, add_entry: &mut dyn FnMut(DirEntry, Entry) -> io::Result<usize>) -> io::Result<()>;
        fn fsyncdir(&self, ctx: &Context, inode: Self::Inode, datasync: bool, handle: Self::Handle) -> io::Result<()>;
        fn releasedir(&self, ctx: &Context, inode: Self::Inode, flags: u32, handle: Self::Handle) -> io::Result<()>;
        fn access(&self, ctx: &Context, inode: Self::Inode, mask: u32) -> io::Result<()>;
        fn lseek(&self, ctx: &Context, inode: Self::Inode, handle: Self::Handle, offset: u64, whence: u32) -> io::Result<u64>;
        fn getlk(&self, ctx: &Context, inode: Self::Inode, handle: Self::Handle, owner: u64, lock: FileLock, flags: u32) -> io::Result<FileLock>;
        fn setlk(&self, ctx: &Context, inode: Self::Inode, handle: Self::Handle, owner: u64, lock: FileLock, flags: u32) -> io::Result<()>;
        fn setlkw(&self, ctx: &Context, inode: Self::Inode, handle: Self::Handle, owner: u64, lock: FileLock, flags: u32) -> io::Result<()>;
        fn ioctl(&self, ctx: &Context, inode: Self::Inode, handle: Self::Handle, flags: u32, cmd: u32, data: IoctlData, out_size: u32) -> io::Result<IoctlData<'_>>;
        fn bmap(&self, ctx: &Context, inode: Self::Inode, block: u64, blocksize: u32) -> io::Result<u64>;
        fn poll(&self, ctx: &Context, inode: Self::Inode, handle: Self::Handle, khandle: Self::Handle, flags: u32, events: u32) -> io::Result<u32>;
        fn notify_reply(&self) -> io::Result<()>;
        fn id_remap(&self, ctx: &mut Context) -> io::Result<()>;
    }
}
