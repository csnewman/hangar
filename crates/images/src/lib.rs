//! The worker's images: pulled from their registries straight into EROFS
//! images, without unpacking them anywhere, and the copies it keeps of
//! them.

mod merge;

pub use merge::{EROFS_ALIGN, Merge, build_erofs};
