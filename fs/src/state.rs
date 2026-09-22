// SPDX-License-Identifier: Apache-2.0

//! What the guest is holding, in a form that survives this process.
//!
//! A guest's FUSE session is mostly state in the backend: the guest refers to
//! files by nodeid, and to mapped regions by an offset in the DAX window, and
//! both mean something only to the process that issued them. A VM restored
//! from a snapshot talks to a backend that has never heard of either, so
//! every operation on a file the guest already had open fails and the guest
//! cannot so much as spawn a process.
//!
//! Two things make rebuilding it possible rather than hopeless. The nodeid a
//! guest holds is `(device slot) << 47 | host inode` -- a function of the file
//! on disk rather than a counter -- so re-looking-up the same path yields the
//! same nodeid, on this boot of the host or the next. And a mapping is fully
//! described by the file it came from and the offsets, so it can be made
//! again through the same call the guest originally made.
//!
//! So this records the path behind every nodeid and the parameters of every
//! live mapping, writes them out on demand, and replays them into a fresh
//! filesystem.

use std::collections::HashMap;
use std::ffi::CString;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

/// The nodeid the FUSE protocol gives the exported directory itself.
pub const ROOT_ID: u64 = 1;

/// A mapping the guest asked for and has not given back.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Mapping {
    pub inode: u64,
    pub foffset: u64,
    pub len: u64,
    pub flags: u64,
    pub moffset: u64,
}

/// The part of a session that has to outlive the process.
#[derive(Default, Serialize, Deserialize)]
pub struct Saved {
    /// nodeid -> path, relative to the exported directory.
    pub inodes: HashMap<u64, PathBuf>,
    /// Live mappings, keyed by their offset in the window.
    pub mappings: HashMap<u64, Mapping>,
}

/// The live record, written to and read from while the guest runs.
#[derive(Default)]
pub struct Session {
    inner: Mutex<Saved>,
}

impl Session {
    pub fn new() -> Self {
        let mut s = Saved::default();
        s.inodes.insert(ROOT_ID, PathBuf::new());
        Self {
            inner: Mutex::new(s),
        }
    }

    /// Remembers where a nodeid came from.
    ///
    /// A nodeid the guest already holds may be handed out again for the same
    /// file, and a path may change under a rename, so the newest answer wins.
    pub fn lookup(&self, parent: u64, name: &std::ffi::CStr, inode: u64) {
        let name = match name.to_str() {
            Ok(n) => n,
            // A name that is not UTF-8 cannot be written out, so the nodeid is
            // left unrecorded and the guest simply loses that one file across
            // a restore rather than the backend losing the whole session.
            Err(_) => return,
        };
        if name == "." || name == ".." {
            return;
        }
        let mut g = self.inner.lock().unwrap();
        let Some(parent_path) = g.inodes.get(&parent).cloned() else {
            return;
        };
        g.inodes.insert(inode, parent_path.join(name));
    }

    /// Records a mapping the guest now holds.
    pub fn map(&self, m: Mapping) {
        let mut g = self.inner.lock().unwrap();
        g.mappings.insert(m.moffset, m);
    }

    /// Drops a mapping the guest has given back.
    pub fn unmap(&self, moffset: u64, len: u64) {
        let mut g = self.inner.lock().unwrap();
        // A removal may cover several mappings at once, so anything starting
        // inside the range goes.
        g.mappings
            .retain(|off, _| !(*off >= moffset && *off < moffset.saturating_add(len)));
    }

    /// Writes the session out.
    ///
    /// Via a temporary file and a rename, so a crash midway leaves the
    /// previous state rather than half of this one.
    pub fn save(&self, path: &Path) -> io::Result<(usize, usize)> {
        let g = self.inner.lock().unwrap();
        let counts = (g.inodes.len(), g.mappings.len());
        let tmp = path.with_extension("tmp");
        let f = std::fs::File::create(&tmp)?;
        serde_json::to_writer(io::BufWriter::new(f), &*g)
            .map_err(|e| io::Error::other(format!("writing the session: {e}")))?;
        std::fs::rename(&tmp, path)?;
        Ok(counts)
    }

    /// Reads a session back.
    pub fn load(path: &Path) -> io::Result<Self> {
        let f = std::fs::File::open(path)?;
        let saved: Saved = serde_json::from_reader(io::BufReader::new(f))
            .map_err(|e| io::Error::other(format!("reading the session: {e}")))?;
        Ok(Self {
            inner: Mutex::new(saved),
        })
    }

    /// The paths to look up again, parents before children.
    ///
    /// Depth order matters: a lookup names a parent nodeid, so a file's
    /// ancestors have to be in the filesystem's own table before it is.
    pub fn replay_order(&self) -> Vec<(u64, PathBuf)> {
        let g = self.inner.lock().unwrap();
        let mut v: Vec<(u64, PathBuf)> = g
            .inodes
            .iter()
            .filter(|(id, _)| **id != ROOT_ID)
            .map(|(id, p)| (*id, p.clone()))
            .collect();
        v.sort_by_key(|(_, p)| p.components().count());
        v
    }

    /// The mappings to make again.
    pub fn mappings(&self) -> Vec<Mapping> {
        let g = self.inner.lock().unwrap();
        let mut v: Vec<Mapping> = g.mappings.values().cloned().collect();
        v.sort_by_key(|m| m.moffset);
        v
    }

    /// Splits a path into the lookups that reach it.
    pub fn components(path: &Path) -> Option<Vec<CString>> {
        path.components()
            .map(|c| CString::new(c.as_os_str().as_encoded_bytes()).ok())
            .collect()
    }
}
