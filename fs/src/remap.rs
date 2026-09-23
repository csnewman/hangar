// SPDX-License-Identifier: Apache-2.0

//! The layer between the guest and the filesystem underneath it.
//!
//! It owns every identifier the guest sees. The filesystem underneath keeps
//! its own nodeids and handles and never shows them to the guest, so they are
//! free to be whatever that implementation finds convenient; what the guest
//! holds is issued here, from a counter, and written down.
//!
//! That is what makes a session survive this process. An identifier the
//! filesystem invents is meaningful only while it is running -- derive it from
//! a counter and a restored backend hands the same number to a different file,
//! derive it from the file and it is only as stable as the inode numbering of
//! whatever the export happens to sit on. Holding the space here removes the
//! question: the identifier is ours, the path behind it is written down, and
//! rebinding the two is a lookup.
//!
//! Every reply that carries an identifier has to pass through here, which is
//! the reason for the bulk below. A nodeid reaches the guest from `lookup`,
//! but also from `create`, `mknod`, `mkdir`, `symlink`, `link` and
//! `readdirplus`; a handle from `open`, `opendir` and `create`. One that is
//! missed is not a crash but a file the guest cannot use again after a
//! restore, which is worse.

use std::collections::HashMap;
use std::ffi::CStr;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
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

use crate::state;

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

/// What the guest has been told about one file.
struct InodeRec {
    inner: Inode,
    path: PathBuf,
    /// How many times the guest has been given this nodeid and not forgotten
    /// it. The filesystem underneath is only told to forget when this reaches
    /// zero, so a nodeid stays valid exactly as long as the guest thinks it
    /// does.
    lookups: u64,
}

/// The identifiers the guest holds, and what they stand for.
#[derive(Default)]
struct Table {
    inodes: HashMap<u64, InodeRec>,
    /// The reverse direction, so a file already known keeps its nodeid rather
    /// than collecting a new one on every lookup.
    by_inner: HashMap<Inode, u64>,
    handles: HashMap<u64, (Handle, state::Handle)>,
    mappings: HashMap<u64, state::Mapping>,
    next_inode: u64,
    next_handle: u64,
}

/// A passthrough filesystem whose identifiers belong to us.
pub struct Remap {
    inner: PassthroughFs,
    table: Mutex<Table>,
    /// What the guest offered in FUSE_INIT; see `state::Saved::capable`.
    capable: Mutex<Option<u64>>,
}

impl Remap {
    pub fn new(inner: PassthroughFs) -> Self {
        let mut t = Table {
            next_inode: state::ROOT_ID + 1,
            next_handle: 1,
            ..Default::default()
        };
        t.inodes.insert(
            state::ROOT_ID,
            InodeRec {
                inner: state::ROOT_ID,
                path: PathBuf::new(),
                lookups: 1,
            },
        );
        t.by_inner.insert(state::ROOT_ID, state::ROOT_ID);
        Self {
            inner,
            table: Mutex::new(t),
            capable: Mutex::new(None),
        }
    }

    /// The nodeid the guest should be given for a file the filesystem has just
    /// named, and the path it was reached through.
    fn intern(&self, t: &mut Table, inner: Inode, path: PathBuf) -> u64 {
        if let Some(&id) = t.by_inner.get(&inner) {
            if let Some(rec) = t.inodes.get_mut(&id) {
                rec.lookups += 1;
                // A rename moves a file without changing what it is, so the
                // newest path is the one to find it by -- but a reply reached
                // through "." or a name that cannot be written down carries
                // no path, and must not erase the one already held.
                if !path.as_os_str().is_empty() {
                    rec.path = path;
                }
            }
            return id;
        }
        let id = t.next_inode;
        t.next_inode += 1;
        t.inodes.insert(
            id,
            InodeRec {
                inner,
                path,
                lookups: 1,
            },
        );
        t.by_inner.insert(inner, id);
        id
    }

    /// Reports a refusal from the filesystem underneath of a nodeid this
    /// layer holds, which means the two tables disagree.
    fn note<T>(&self, op: &str, id: u64, r: io::Result<T>) -> io::Result<T> {
        if let Err(e) = &r {
            if e.raw_os_error() == Some(libc::EBADF) {
                let inner = self.table.lock().unwrap().inodes.get(&id).map(|r| r.inner);
                log::warn!(
                    "{op}: nodeid {id} (inner {inner:?}) refused by the filesystem underneath"
                );
            }
        }
        r
    }

    /// The filesystem's own nodeid for one the guest named.
    fn inode(&self, id: u64) -> io::Result<Inode> {
        let t = self.table.lock().unwrap();
        t.inodes.get(&id).map(|r| r.inner).ok_or_else(|| {
            log::warn!("the guest named nodeid {id}, which was never issued here");
            io::Error::from_raw_os_error(libc::EBADF)
        })
    }

    fn path_of(&self, t: &Table, id: u64) -> Option<PathBuf> {
        t.inodes.get(&id).map(|r| r.path.clone())
    }

    /// The filesystem's own handle for one the guest named.
    ///
    /// With `no_open` the guest is never given a handle, and the kernel fills
    /// the field with a sentinel instead -- zero, or all ones where it means
    /// "there is no open file". Neither was issued here, so both pass through
    /// for the filesystem underneath to interpret, which is what it expects
    /// when it told the guest not to open anything.
    fn handle(&self, h: u64) -> io::Result<Handle> {
        if h == 0 || h == u64::MAX {
            return Ok(h);
        }
        let t = self.table.lock().unwrap();
        t.handles.get(&h).map(|(inner, _)| *inner).ok_or_else(|| {
            log::warn!("the guest named handle {h}, which was never issued here");
            io::Error::from_raw_os_error(libc::EBADF)
        })
    }

    fn intern_handle(
        &self,
        inner: Option<Handle>,
        inode: u64,
        flags: u32,
        dir: bool,
    ) -> Option<u64> {
        let inner = inner?;
        let mut t = self.table.lock().unwrap();
        let id = t.next_handle;
        t.next_handle += 1;
        t.handles
            .insert(id, (inner, state::Handle { inode, flags, dir }));
        Some(id)
    }

    /// Rewrites a reply so it names the guest's nodeid rather than the
    /// filesystem's.
    fn entry_out(&self, mut e: Entry, parent: u64, name: &CStr) -> Entry {
        // Zero is how the protocol says a name does not exist, cached for as
        // long as the reply's timeout. Giving it an identifier would tell the
        // guest the opposite.
        if e.inode == 0 {
            return e;
        }
        let mut t = self.table.lock().unwrap();
        let path = match (self.path_of(&t, parent), name.to_str()) {
            (Some(p), Ok(n)) if n != "." && n != ".." => p.join(n),
            // Without a path this file cannot be found again, so it gets a
            // nodeid that works now and is left out of what is written down.
            _ => PathBuf::new(),
        };
        e.inode = self.intern(&mut t, e.inode, path);
        e
    }

    /// Everything the guest is holding, for writing out.
    pub fn saved(&self) -> state::Saved {
        let t = self.table.lock().unwrap();
        let mut s = state::Saved::new();
        s.next_inode = t.next_inode;
        s.next_handle = t.next_handle;
        s.capable = *self.capable.lock().unwrap();
        for (id, rec) in &t.inodes {
            // A file with no path cannot be looked up again, so recording it
            // would only promise something the restore cannot keep.
            if *id == state::ROOT_ID || rec.path.as_os_str().is_empty() {
                continue;
            }
            s.inodes.insert(*id, rec.path.clone());
        }
        for (id, (_, h)) in &t.handles {
            s.handles.insert(*id, h.clone());
        }
        s.mappings = t.mappings.clone();
        s
    }

    /// The mappings a restored guest is still holding.
    pub fn pending_mappings(&self) -> Vec<state::Mapping> {
        let t = self.table.lock().unwrap();
        let mut v: Vec<state::Mapping> = t.mappings.values().cloned().collect();
        v.sort_by_key(|m| m.moffset);
        v
    }

    /// Binds the identifiers a guest is still using to the files behind them.
    ///
    /// A path that has gone is skipped: losing one file is better than
    /// refusing to bring the guest back, and it fails for the guest exactly as
    /// it would have without a restore.
    pub fn restore(&self, saved: &state::Saved) -> (usize, usize) {
        let ctx = Context::default();
        let (mut ok, mut gone) = (0usize, 0usize);
        // Before anything else: the options the guest negotiated decide how
        // the filesystem answers every request after this, and the guest will
        // not negotiate them again.
        if let Some(bits) = saved.capable {
            if let Err(e) = self.init(FsOptions::from_bits_truncate(bits)) {
                log::error!("replaying the guest's FUSE_INIT: {e}");
            }
        }
        {
            let mut t = self.table.lock().unwrap();
            t.next_inode = saved.next_inode.max(state::ROOT_ID + 1);
            t.next_handle = saved.next_handle.max(1);
        }
        for (id, path) in saved.inode_order() {
            match self.find(&ctx, &path) {
                Ok(inner) => {
                    let mut t = self.table.lock().unwrap();
                    t.inodes.insert(
                        id,
                        InodeRec {
                            inner,
                            path,
                            lookups: 1,
                        },
                    );
                    t.by_inner.insert(inner, id);
                    ok += 1;
                }
                Err(_) => gone += 1,
            }
        }
        for (id, h) in &saved.handles {
            let Ok(inner_inode) = self.inode(h.inode) else {
                continue;
            };
            let opened = if h.dir {
                self.inner
                    .opendir(&ctx, inner_inode, h.flags)
                    .map(|(x, _)| x)
            } else {
                self.inner
                    .open(&ctx, inner_inode, h.flags, 0)
                    .map(|(x, _, _)| x)
            };
            if let Ok(Some(inner)) = opened {
                let mut t = self.table.lock().unwrap();
                t.handles.insert(*id, (inner, h.clone()));
            }
        }
        {
            let mut t = self.table.lock().unwrap();
            t.mappings = saved.mappings.clone();
        }
        (ok, gone)
    }

    /// Walks a path, so the filesystem underneath holds the file again.
    fn find(&self, ctx: &Context, path: &Path) -> io::Result<Inode> {
        let parts = state::Saved::components(path)
            .ok_or_else(|| io::Error::other("a path that cannot be looked up"))?;
        let mut inner = state::ROOT_ID;
        for part in parts {
            inner = self.inner.lookup(ctx, inner, &part)?.inode;
        }
        Ok(inner)
    }

    /// Makes a mapping the guest already believes in.
    pub fn remap(
        &self,
        ctx: &Context,
        m: &state::Mapping,
        vu_req: &mut dyn FsCacheReqHandler,
    ) -> io::Result<()> {
        let inode = self.inode(m.inode)?;
        self.inner
            .setupmapping(ctx, inode, 0, m.foffset, m.len, m.flags, m.moffset, vu_req)
    }
}

type Inode = <PassthroughFs as FileSystem>::Inode;
type Handle = <PassthroughFs as FileSystem>::Handle;

/// Forward a call, translating the nodeid the guest named.
macro_rules! by_inode {
    ($( fn $name:ident ( &self, ctx: &Context, inode $(, $arg:ident : $ty:ty )* $(,)? ) -> $ret:ty ; )*) => {
        $(
            fn $name(&self, ctx: &Context, inode: Self::Inode $(, $arg: $ty)*) -> $ret {
                let id = inode;
                let inode = self.inode(inode)?;
                self.note(stringify!($name), id, self.inner.$name(ctx, inode $(, $arg)*))
            }
        )*
    };
}

/// Forward a call, translating the nodeid and the handle.
macro_rules! by_inode_handle {
    ($( fn $name:ident ( &self, ctx: &Context, inode, handle $(, $arg:ident : $ty:ty )* $(,)? ) -> $ret:ty ; )*) => {
        $(
            fn $name(&self, ctx: &Context, inode: Self::Inode, handle: Self::Handle $(, $arg: $ty)*) -> $ret {
                let id = inode;
                let inode = self.inode(inode)?;
                let handle = self.handle(handle)?;
                self.note(stringify!($name), id, self.inner.$name(ctx, inode, handle $(, $arg)*))
            }
        )*
    };
}

impl FileSystem for Remap {
    type Inode = u64;
    type Handle = u64;

    fn init(&self, capable: FsOptions) -> io::Result<FsOptions> {
        *self.capable.lock().unwrap() = Some(capable.bits());
        // `capable` is what the guest offered. Asking for anything outside it
        // would be dropped, so the union is intersected before it is
        // returned.
        Ok((self.inner.init(capable)? | transport_options()) & capable)
    }

    fn destroy(&self) {
        self.inner.destroy()
    }

    // --- replies that carry a nodeid ------------------------------------

    fn lookup(&self, ctx: &Context, parent: Self::Inode, name: &CStr) -> io::Result<Entry> {
        let inner_parent = self.inode(parent)?;
        let entry = self.note("lookup", parent, self.inner.lookup(ctx, inner_parent, name))?;
        Ok(self.entry_out(entry, parent, name))
    }

    fn mknod(
        &self,
        ctx: &Context,
        parent: Self::Inode,
        name: &CStr,
        mode: u32,
        rdev: u32,
        umask: u32,
    ) -> io::Result<Entry> {
        let inner_parent = self.inode(parent)?;
        let entry = self
            .inner
            .mknod(ctx, inner_parent, name, mode, rdev, umask)?;
        Ok(self.entry_out(entry, parent, name))
    }

    fn mkdir(
        &self,
        ctx: &Context,
        parent: Self::Inode,
        name: &CStr,
        mode: u32,
        umask: u32,
    ) -> io::Result<Entry> {
        let inner_parent = self.inode(parent)?;
        let entry = self.inner.mkdir(ctx, inner_parent, name, mode, umask)?;
        Ok(self.entry_out(entry, parent, name))
    }

    fn symlink(
        &self,
        ctx: &Context,
        linkname: &CStr,
        parent: Self::Inode,
        name: &CStr,
    ) -> io::Result<Entry> {
        let inner_parent = self.inode(parent)?;
        let entry = self.inner.symlink(ctx, linkname, inner_parent, name)?;
        Ok(self.entry_out(entry, parent, name))
    }

    fn link(
        &self,
        ctx: &Context,
        inode: Self::Inode,
        newparent: Self::Inode,
        newname: &CStr,
    ) -> io::Result<Entry> {
        let inner = self.inode(inode)?;
        let inner_parent = self.inode(newparent)?;
        let entry = self.inner.link(ctx, inner, inner_parent, newname)?;
        Ok(self.entry_out(entry, newparent, newname))
    }

    fn create(
        &self,
        ctx: &Context,
        parent: Self::Inode,
        name: &CStr,
        args: CreateIn,
    ) -> io::Result<(Entry, Option<Self::Handle>, OpenOptions, Option<u32>)> {
        let inner_parent = self.inode(parent)?;
        let (entry, handle, opts, extra) = self.inner.create(ctx, inner_parent, name, args)?;
        let entry = self.entry_out(entry, parent, name);
        let handle = self.intern_handle(handle, entry.inode, args.flags, false);
        Ok((entry, handle, opts, extra))
    }

    fn readdirplus(
        &self,
        ctx: &Context,
        inode: Self::Inode,
        handle: Self::Handle,
        size: u32,
        offset: u64,
        add_entry: &mut dyn FnMut(DirEntry, Entry) -> io::Result<usize>,
    ) -> io::Result<()> {
        let inner = self.inode(inode)?;
        let inner_handle = self.handle(handle)?;
        // A nodeid reaches the guest here exactly as it does from a lookup,
        // and has to be ours for the same reason.
        self.inner
            .readdirplus(ctx, inner, inner_handle, size, offset, &mut |de, entry| {
                let name = std::ffi::CString::new(de.name)
                    .map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
                let entry = self.entry_out(entry, inode, &name);
                add_entry(de, entry)
            })
    }

    // --- replies that carry a handle ------------------------------------

    fn open(
        &self,
        ctx: &Context,
        inode: Self::Inode,
        flags: u32,
        fuse_flags: u32,
    ) -> io::Result<(Option<Self::Handle>, OpenOptions, Option<u32>)> {
        let inner = self.inode(inode)?;
        let (handle, opts, extra) = self.note(
            "open",
            inode,
            self.inner.open(ctx, inner, flags, fuse_flags),
        )?;
        Ok((self.intern_handle(handle, inode, flags, false), opts, extra))
    }

    fn opendir(
        &self,
        ctx: &Context,
        inode: Self::Inode,
        flags: u32,
    ) -> io::Result<(Option<Self::Handle>, OpenOptions)> {
        let inner = self.inode(inode)?;
        let (handle, opts) = self.inner.opendir(ctx, inner, flags)?;
        Ok((self.intern_handle(handle, inode, flags, true), opts))
    }

    fn release(
        &self,
        ctx: &Context,
        inode: Self::Inode,
        flags: u32,
        handle: Self::Handle,
        flush: bool,
        flock_release: bool,
        lock_owner: Option<u64>,
    ) -> io::Result<()> {
        let inner = self.inode(inode)?;
        let inner_handle = self.handle(handle)?;
        self.table.lock().unwrap().handles.remove(&handle);
        self.inner.release(
            ctx,
            inner,
            flags,
            inner_handle,
            flush,
            flock_release,
            lock_owner,
        )
    }

    fn releasedir(
        &self,
        ctx: &Context,
        inode: Self::Inode,
        flags: u32,
        handle: Self::Handle,
    ) -> io::Result<()> {
        let inner = self.inode(inode)?;
        let inner_handle = self.handle(handle)?;
        self.table.lock().unwrap().handles.remove(&handle);
        self.inner.releasedir(ctx, inner, flags, inner_handle)
    }

    // --- the guest giving nodeids back ----------------------------------

    fn forget(&self, ctx: &Context, inode: Self::Inode, count: u64) {
        let mut t = self.table.lock().unwrap();
        let Some(rec) = t.inodes.get_mut(&inode) else {
            return;
        };
        rec.lookups = rec.lookups.saturating_sub(count);
        if rec.lookups > 0 {
            return;
        }
        let inner = rec.inner;
        t.inodes.remove(&inode);
        t.by_inner.remove(&inner);
        drop(t);
        self.inner.forget(ctx, inner, count)
    }

    fn batch_forget(&self, ctx: &Context, requests: Vec<(Self::Inode, u64)>) {
        for (inode, count) in requests {
            self.forget(ctx, inode, count);
        }
    }

    // --- mappings --------------------------------------------------------

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
        let inner = self.inode(inode)?;
        let inner_handle = self.handle(handle)?;
        self.note(
            "setupmapping",
            inode,
            self.inner.setupmapping(
                ctx,
                inner,
                inner_handle,
                foffset,
                len,
                flags,
                moffset,
                vu_req,
            ),
        )?;
        self.table.lock().unwrap().mappings.insert(
            moffset,
            state::Mapping {
                inode,
                foffset,
                len,
                flags,
                moffset,
            },
        );
        Ok(())
    }

    fn removemapping(
        &self,
        ctx: &Context,
        inode: Self::Inode,
        requests: Vec<RemovemappingOne>,
        vu_req: &mut dyn FsCacheReqHandler,
    ) -> io::Result<()> {
        let inner = self.inode(inode)?;
        {
            let mut t = self.table.lock().unwrap();
            for r in &requests {
                // One removal may cover several mappings, so anything
                // starting inside the range goes.
                let end = r.moffset.saturating_add(r.len);
                t.mappings
                    .retain(|off, _| !(*off >= r.moffset && *off < end));
            }
        }
        self.inner.removemapping(ctx, inner, requests, vu_req)
    }

    // --- two parents ------------------------------------------------------

    fn unlink(&self, ctx: &Context, parent: Self::Inode, name: &CStr) -> io::Result<()> {
        let inner = self.inode(parent)?;
        self.inner.unlink(ctx, inner, name)
    }

    fn rmdir(&self, ctx: &Context, parent: Self::Inode, name: &CStr) -> io::Result<()> {
        let inner = self.inode(parent)?;
        self.inner.rmdir(ctx, inner, name)
    }

    fn rename(
        &self,
        ctx: &Context,
        olddir: Self::Inode,
        oldname: &CStr,
        newdir: Self::Inode,
        newname: &CStr,
        flags: u32,
    ) -> io::Result<()> {
        let inner_old = self.inode(olddir)?;
        let inner_new = self.inode(newdir)?;
        self.inner
            .rename(ctx, inner_old, oldname, inner_new, newname, flags)?;
        // The file keeps its nodeid, so the path behind it has to follow it to
        // its new name, which is where a restore will look for it.
        let mut t = self.table.lock().unwrap();
        if let (Some(from), Some(to), Ok(old), Ok(new)) = (
            self.path_of(&t, olddir),
            self.path_of(&t, newdir),
            oldname.to_str(),
            newname.to_str(),
        ) {
            let (from, to) = (from.join(old), to.join(new));
            for rec in t.inodes.values_mut() {
                if rec.path == from {
                    rec.path = to.clone();
                } else if let Ok(rest) = rec.path.strip_prefix(&from) {
                    rec.path = to.join(rest);
                }
            }
        }
        Ok(())
    }

    // --- plain forwards ---------------------------------------------------

    fn readdir(
        &self,
        ctx: &Context,
        inode: Self::Inode,
        handle: Self::Handle,
        size: u32,
        offset: u64,
        add_entry: &mut dyn FnMut(DirEntry) -> io::Result<usize>,
    ) -> io::Result<()> {
        let inner = self.inode(inode)?;
        let inner_handle = self.handle(handle)?;
        self.inner
            .readdir(ctx, inner, inner_handle, size, offset, add_entry)
    }

    fn notify_reply(&self) -> io::Result<()> {
        self.inner.notify_reply()
    }

    fn id_remap(&self, ctx: &mut Context) -> io::Result<()> {
        self.inner.id_remap(ctx)
    }

    by_inode! {
        fn getattr(&self, ctx: &Context, inode, handle: Option<Self::Handle>) -> io::Result<(stat64, Duration)>;
        fn setattr(&self, ctx: &Context, inode, attr: stat64, handle: Option<Self::Handle>, valid: SetattrValid) -> io::Result<(stat64, Duration)>;
        fn readlink(&self, ctx: &Context, inode) -> io::Result<Vec<u8>>;
        fn statfs(&self, ctx: &Context, inode) -> io::Result<statvfs64>;
        fn setxattr(&self, ctx: &Context, inode, name: &CStr, value: &[u8], flags: u32) -> io::Result<()>;
        fn getxattr(&self, ctx: &Context, inode, name: &CStr, size: u32) -> io::Result<GetxattrReply>;
        fn listxattr(&self, ctx: &Context, inode, size: u32) -> io::Result<ListxattrReply>;
        fn removexattr(&self, ctx: &Context, inode, name: &CStr) -> io::Result<()>;
        fn access(&self, ctx: &Context, inode, mask: u32) -> io::Result<()>;
        fn bmap(&self, ctx: &Context, inode, block: u64, blocksize: u32) -> io::Result<u64>;
    }

    by_inode_handle! {
        fn read(&self, ctx: &Context, inode, handle, w: &mut dyn ZeroCopyWriter, size: u32, offset: u64, lock_owner: Option<u64>, flags: u32) -> io::Result<usize>;
        fn write(&self, ctx: &Context, inode, handle, r: &mut dyn ZeroCopyReader, size: u32, offset: u64, lock_owner: Option<u64>, delayed_write: bool, flags: u32, fuse_flags: u32) -> io::Result<usize>;
        fn flush(&self, ctx: &Context, inode, handle, lock_owner: u64) -> io::Result<()>;
        fn fallocate(&self, ctx: &Context, inode, handle, mode: u32, offset: u64, length: u64) -> io::Result<()>;
        fn lseek(&self, ctx: &Context, inode, handle, offset: u64, whence: u32) -> io::Result<u64>;
        fn getlk(&self, ctx: &Context, inode, handle, owner: u64, lock: FileLock, flags: u32) -> io::Result<FileLock>;
        fn setlk(&self, ctx: &Context, inode, handle, owner: u64, lock: FileLock, flags: u32) -> io::Result<()>;
        fn setlkw(&self, ctx: &Context, inode, handle, owner: u64, lock: FileLock, flags: u32) -> io::Result<()>;
        fn ioctl(&self, ctx: &Context, inode, handle, flags: u32, cmd: u32, data: IoctlData, out_size: u32) -> io::Result<IoctlData<'_>>;
    }

    // `datasync` sits between the nodeid and the handle in these two, so they
    // do not fit either shape above.
    fn fsync(
        &self,
        ctx: &Context,
        inode: Self::Inode,
        datasync: bool,
        handle: Self::Handle,
    ) -> io::Result<()> {
        let inner = self.inode(inode)?;
        let inner_handle = self.handle(handle)?;
        self.inner.fsync(ctx, inner, datasync, inner_handle)
    }

    fn fsyncdir(
        &self,
        ctx: &Context,
        inode: Self::Inode,
        datasync: bool,
        handle: Self::Handle,
    ) -> io::Result<()> {
        let inner = self.inode(inode)?;
        let inner_handle = self.handle(handle)?;
        self.inner.fsyncdir(ctx, inner, datasync, inner_handle)
    }

    fn poll(
        &self,
        ctx: &Context,
        inode: Self::Inode,
        handle: Self::Handle,
        khandle: Self::Handle,
        flags: u32,
        events: u32,
    ) -> io::Result<u32> {
        let inner = self.inode(inode)?;
        let inner_handle = self.handle(handle)?;
        let inner_khandle = self.handle(khandle)?;
        self.inner
            .poll(ctx, inner, inner_handle, inner_khandle, flags, events)
    }
}
