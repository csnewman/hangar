//! The worker's images: pulled from their registries straight into EROFS
//! images, without unpacking them anywhere, and the copies it keeps of
//! them.

mod merge;
mod reference;
mod registry;

pub use merge::{EROFS_ALIGN, Merge, build_erofs};
pub use reference::Reference;
pub use registry::{Auth, Client, HangarRegistry, Progress, Stage, guest_arch};
