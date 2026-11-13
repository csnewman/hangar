//! EROFS images: the read-only filesystem an environment's image is
//! attached as.
//!
//! The images [`Writer`] makes are uncompressed, with 4 KiB blocks, so a
//! guest can map a file's blocks straight from the host's page cache (DAX).
//! A file's data is written as it arrives, so an image is built from a
//! stream -- a tar archive, or a directory walked once -- without unpacking
//! it anywhere; what describes the tree (inodes, directories, the last part
//! of each small file) is laid out once everything has arrived, after the
//! data. The on-disk format is Linux's, fs/erofs/erofs_fs.h.
//!
//! [`Image`] reads them, and the uncompressed images mkfs.erofs makes, for
//! hosts that cannot mount EROFS.

mod format;
mod reader;
mod writer;

#[cfg(unix)]
mod dir;

pub use format::{BLOCK_SIZE, Kind};
pub use reader::{DirEntry, Image, Inode};
pub use writer::{Entry, Out, Writer};
