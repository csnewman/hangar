# Hangar's changes to fuser 0.18.0

This is fuser 0.18.0 from crates.io, without its test harnesses, with these
changes for hangar-router:

- **`flock()` locks are told apart from POSIX locks.** The kernel sends both
  as `FUSE_SETLK`/`FUSE_SETLKW`, marking `flock()` with `FUSE_LK_FLOCK` in
  `lk_flags`, which fuser read and dropped. `Filesystem::setlk` takes a
  `flock` argument.

Known, not changed:

- **`FUSE_SETXATTR_EXT`** is accepted, but `SETXATTR` is still parsed with
  the old, shorter header, so the value's length check fails and the worker
  thread panics. hangar-router does not ask for it.

Still to add:

- **`FUSE_TMPFILE`** (opcode 51), for `O_TMPFILE` and overlayfs's copy-ups.
