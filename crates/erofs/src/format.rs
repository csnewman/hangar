//! The on-disk format's numbers, as Linux's fs/erofs/erofs_fs.h has them.

/// The block size of the images [`crate::Writer`] makes.
pub const BLOCK_SIZE: u64 = 4096;

pub(crate) const BLOCK_BITS: u8 = 12;
pub(crate) const SUPER_OFFSET: u64 = 1024;
pub(crate) const SUPER_MAGIC: u32 = 0xE0F5_E1E2;
pub(crate) const SUPER_SIZE: usize = 128;

/// An inode's number (nid) is its offset from the metadata area's start in
/// 32-byte slots.
pub(crate) const SLOT_BITS: u32 = 5;
pub(crate) const SLOT_SIZE: u64 = 1 << SLOT_BITS;
pub(crate) const COMPACT_SIZE: usize = 32;
pub(crate) const EXTENDED_SIZE: usize = 64;

/// i_format: bit 0 is the inode's version, bits 1-3 its data layout.
pub(crate) const VERSION_EXTENDED: u16 = 1;
pub(crate) const LAYOUT_PLAIN: u8 = 0;
pub(crate) const LAYOUT_INLINE: u8 = 2;

pub(crate) const NULL_ADDR: u32 = 0xFFFF_FFFF;

pub(crate) const DIRENT_SIZE: usize = 12;
pub(crate) const MAX_NAME: usize = 255;

pub(crate) const XATTR_HEADER_SIZE: usize = 12;
pub(crate) const XATTR_ENTRY_SIZE: usize = 4;
pub(crate) const MAX_XATTR_VALUE: usize = 0xFFFF;

/// What an entry is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Regular,
    Dir,
    Symlink,
    CharDevice,
    BlockDevice,
    Fifo,
    Socket,
}

impl Kind {
    /// The S_IFMT bits of its mode.
    pub(crate) fn mode(self) -> u16 {
        match self {
            Kind::Regular => 0o100000,
            Kind::Dir => 0o040000,
            Kind::Symlink => 0o120000,
            Kind::CharDevice => 0o020000,
            Kind::BlockDevice => 0o060000,
            Kind::Fifo => 0o010000,
            Kind::Socket => 0o140000,
        }
    }

    pub(crate) fn of_mode(mode: u16) -> Option<Kind> {
        Some(match mode & 0o170000 {
            0o100000 => Kind::Regular,
            0o040000 => Kind::Dir,
            0o120000 => Kind::Symlink,
            0o020000 => Kind::CharDevice,
            0o060000 => Kind::BlockDevice,
            0o010000 => Kind::Fifo,
            0o140000 => Kind::Socket,
            _ => return None,
        })
    }

    /// The file type a directory entry records, as Linux's FT_* values.
    pub(crate) fn dirent(self) -> u8 {
        match self {
            Kind::Regular => 1,
            Kind::Dir => 2,
            Kind::CharDevice => 3,
            Kind::BlockDevice => 4,
            Kind::Fifo => 5,
            Kind::Socket => 6,
            Kind::Symlink => 7,
        }
    }

    pub(crate) fn of_dirent(ft: u8) -> Option<Kind> {
        Some(match ft {
            1 => Kind::Regular,
            2 => Kind::Dir,
            3 => Kind::CharDevice,
            4 => Kind::BlockDevice,
            5 => Kind::Fifo,
            6 => Kind::Socket,
            7 => Kind::Symlink,
            _ => return None,
        })
    }
}

impl std::fmt::Display for Kind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Kind::Regular => "file",
            Kind::Dir => "directory",
            Kind::Symlink => "symlink",
            Kind::CharDevice => "character device",
            Kind::BlockDevice => "block device",
            Kind::Fifo => "fifo",
            Kind::Socket => "socket",
        })
    }
}

/// A device number as Linux's new_encode_dev packs it.
pub(crate) fn encode_dev(major: u32, minor: u32) -> u32 {
    (minor & 0xff) | (major << 8) | ((minor & !0xff) << 12)
}

pub(crate) fn decode_dev(dev: u32) -> (u32, u32) {
    ((dev & 0xfff00) >> 8, (dev & 0xff) | ((dev >> 12) & 0xfff00))
}

/// The prefixes an extended attribute's name is stored by: an index and the
/// rest of the name. The ACLs are whole names, so they come before the
/// prefixes they share.
pub(crate) const XATTR_PREFIXES: [(u8, &str); 6] = [
    (2, "system.posix_acl_access"),
    (3, "system.posix_acl_default"),
    (1, "user."),
    (4, "trusted."),
    (5, "lustre."),
    (6, "security."),
];

pub(crate) fn xattr_prefix(index: u8) -> Option<&'static str> {
    XATTR_PREFIXES
        .iter()
        .find(|(i, _)| *i == index)
        .map(|(_, p)| *p)
}

pub(crate) fn align(n: u64, to: u64) -> u64 {
    n.div_ceil(to) * to
}

/// The size of an inode's inline attributes, from its i_xattr_icount.
pub(crate) fn xattr_body_size(icount: u16) -> u64 {
    if icount == 0 {
        0
    } else {
        (XATTR_HEADER_SIZE + XATTR_ENTRY_SIZE * (icount as usize - 1)) as u64
    }
}

pub(crate) fn le16(b: &[u8]) -> u16 {
    u16::from_le_bytes([b[0], b[1]])
}

pub(crate) fn le32(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

pub(crate) fn le64(b: &[u8]) -> u64 {
    u64::from_le_bytes(b[..8].try_into().unwrap())
}
