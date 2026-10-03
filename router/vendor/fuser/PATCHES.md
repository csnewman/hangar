# Hangar's changes to fuser 0.18.0

This is fuser 0.18.0 from crates.io, without its test harnesses, with these
changes for hangar-router:

- **`flock()` locks are told apart from POSIX locks.** The kernel sends both
  as `FUSE_SETLK`/`FUSE_SETLKW`, marking `flock()` with `FUSE_LK_FLOCK` in
  `lk_flags`, which fuser read and dropped. `Filesystem::setlk` takes a
  `flock` argument.
- **FUSE over io_uring** (`src/uring.rs`, `Config::io_uring`). With it, a
  session also runs one io_uring ring per possible CPU, each on a thread
  pinned to that CPU, and the kernel sends each request to the ring of the
  CPU its caller runs on, once every ring has registered. A request is
  rebuilt in front of its payload, where fuser's parser expects it, and
  `ReplySender::Ring` holds the reply until the method returns, when the
  thread commits it and fetches the next request in one command. Replies
  must therefore be sent before the method returns. Forgets and interrupts
  still come through `/dev/fuse`, and so does everything if the kernel
  refuses the rings. Adds the `io-uring` crate.

Known, not changed:

- **`FUSE_SETXATTR_EXT`** is accepted, but `SETXATTR` is still parsed with
  the old, shorter header, so the value's length check fails and the worker
  thread panics. hangar-router does not ask for it.

Still to add:

- **`FUSE_TMPFILE`** (opcode 51), for `O_TMPFILE` and overlayfs's copy-ups.
