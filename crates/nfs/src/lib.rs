//! An NFSv4.1 and 4.2 server for one environment's share of the files
//! environments share with others: their file sets, each a directory of a
//! root (View). It serves the operations Linux's client uses, each a system
//! call on the root made as the caller -- the client's user as it says it
//! is (AUTH_SYS), squashed as the set says (Squash) -- so the kernel checks
//! permissions and owns new files as it would for them.
//!
//! Locks are open file description locks on the root's files, one
//! description for each lock owner's locks on a file: they hold between
//! this server's clients and anything else locking the same files, every
//! environment on the worker, and through an NFS mount of the root, on
//! every worker. A client that stops renewing its lease loses its state,
//! its opens and so its locks, as a crashed environment should. There are
//! no delegations, and no state outlives the server: an environment's
//! server lives as long as the environment does on its worker.

// The system's types differ between Linux and macOS (mode_t is 32 bits on
// one, 16 on the other), so a cast needed on one is a no-op on the other.
#![allow(clippy::unnecessary_cast)]

mod attr;
mod fs;
mod ops;
mod proto;
mod rpc;
mod server;
mod state;
mod sys;
mod xdr;

pub use server::{Config, Server, Squash, Stream, View};
