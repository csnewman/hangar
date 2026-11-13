//! The worker's images: pulled from their registries straight into EROFS
//! images, without unpacking them anywhere, and the copies it keeps of
//! them.

mod merge;
mod reference;
mod registry;
#[cfg(unix)]
mod store;

pub use merge::{EROFS_ALIGN, Merge, build_erofs};
pub use reference::Reference;
pub use registry::{Auth, Client, HangarRegistry, Progress, Stage, guest_arch};
#[cfg(unix)]
pub use store::{
    EROFS_FILE, IMAGE_INFO_PATH, Image, ImageCopy, ImageInfo, LocalImage, Store, allocated,
    build_of, fingerprint, read_image_info, read_image_info_erofs,
};
