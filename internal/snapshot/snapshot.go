// Package snapshot pulls OCI images and unpacks them into overlayfs
// snapshots, so an environment's read-only base layer exists on the host as
// an ordinary directory.
//
// That directory is what virtiofs exports to a guest. Nothing is converted
// into a disk image: a flattened ext4 blob cannot share layers between
// environments, cannot share a page cache, and has to be rebuilt whenever the
// image changes. See docs/image-runtime-format.md section 4.
//
// containerd is used as a library. There is no containerd daemon, no gRPC and
// no Docker -- the snapshotter, the content store, the applier and the
// registry client are all constructors that take a directory and return an
// interface.
package snapshot

// DefaultPlatform is the platform matched when pulling, if the caller does
// not say. A guest runs the host's architecture, so this is not a choice a
// worker gets to make.
const DefaultPlatform = ""
