// SPDX-License-Identifier: Apache-2.0

//! The identifiers the guest holds, in a form that survives this process.
//!
//! A guest's FUSE session is mostly state in the backend. The guest refers to
//! files by nodeid, to open files by handle, and to mapped regions by an
//! offset in the DAX window, and all three mean something only to the process
//! that issued them. A VM restored from a snapshot talks to a backend that has
//! never heard of any of them, so every operation on a file it already had
//! open fails and it cannot so much as spawn a process.
//!
//! The identifiers are therefore issued here rather than by the filesystem
//! underneath, which keeps its own and never shows them to the guest. Ours are
//! a counter, so they can be written down and handed back out; a file is found
//! again by the path it was reached through, and a mapping by the file and the
//! offsets it was made from.

use std::collections::HashMap;
use std::ffi::CString;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The nodeid the FUSE protocol gives the exported directory itself.
pub const ROOT_ID: u64 = 1;

/// A mapping the guest asked for and has not given back.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Mapping {
    /// The nodeid as the guest knows it.
    pub inode: u64,
    pub foffset: u64,
    pub len: u64,
    pub flags: u64,
    pub moffset: u64,
}

/// An open file the guest is holding.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Handle {
    /// The nodeid as the guest knows it.
    pub inode: u64,
    pub flags: u32,
    pub dir: bool,
}

/// Everything that has to outlive the process.
#[derive(Default, Serialize, Deserialize)]
pub struct Saved {
    /// nodeid -> path, relative to the exported directory.
    pub inodes: HashMap<u64, PathBuf>,
    /// handle -> what it was opened as.
    pub handles: HashMap<u64, Handle>,
    /// Live mappings, keyed by their offset in the window.
    pub mappings: HashMap<u64, Mapping>,
    /// Where the counters had got to, so a restored session never issues an
    /// identifier the guest is already using for something else.
    pub next_inode: u64,
    pub next_handle: u64,
}

impl Saved {
    pub fn new() -> Self {
        let mut s = Saved {
            next_inode: ROOT_ID + 1,
            next_handle: 1,
            ..Default::default()
        };
        s.inodes.insert(ROOT_ID, PathBuf::new());
        s
    }

    /// Writes the session out, via a temporary file and a rename so a crash
    /// midway leaves the previous state rather than half of this one.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let tmp = path.with_extension("tmp");
        let f = std::fs::File::create(&tmp)?;
        serde_json::to_writer(io::BufWriter::new(f), self)
            .map_err(|e| io::Error::other(format!("writing the session: {e}")))?;
        std::fs::rename(&tmp, path)
    }

    /// Reads a session back.
    pub fn load(path: &Path) -> io::Result<Self> {
        let f = std::fs::File::open(path)?;
        serde_json::from_reader(io::BufReader::new(f))
            .map_err(|e| io::Error::other(format!("reading the session: {e}")))
    }

    /// The paths to find again, parents before children.
    ///
    /// Depth order matters: a lookup names a parent, so a file's ancestors
    /// have to be known before it is.
    pub fn inode_order(&self) -> Vec<(u64, PathBuf)> {
        let mut v: Vec<(u64, PathBuf)> = self
            .inodes
            .iter()
            .filter(|(id, _)| **id != ROOT_ID)
            .map(|(id, p)| (*id, p.clone()))
            .collect();
        v.sort_by_key(|(_, p)| p.components().count());
        v
    }

    /// Splits a path into the lookups that reach it.
    pub fn components(path: &Path) -> Option<Vec<CString>> {
        path.components()
            .map(|c| CString::new(c.as_os_str().as_encoded_bytes()).ok())
            .collect()
    }
}
