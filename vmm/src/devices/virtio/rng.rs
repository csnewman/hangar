//! virtio-rng, the guest's source of entropy.
//!
//! Without one the guest has only what its own architecture offers, and
//! anything that wants randomness early -- a machine ID, a host key, a TLS
//! handshake -- either falls back to something weaker or waits. The device
//! costs one queue and a call to the host's random number generator, so there
//! is no reason for an environment not to have it.

use std::io;
use std::sync::Arc;

use vm_memory::Bytes;

use super::worker::{self, Stop};
use super::{ActiveQueue, Interrupt, VirtioDevice, TYPE_RNG};
use crate::memory::Mem;

const QUEUE_SIZE: u16 = 8;

/// The guest asks in small amounts; this is far above anything it requests in
/// one go, and bounds what one malformed descriptor can make this allocate.
const MAX_REQUEST: usize = 64 * 1024;

pub struct Rng {
    stop: Stop,
}

impl Rng {
    pub fn new(stop: Stop) -> Self {
        Self { stop }
    }
}

impl VirtioDevice for Rng {
    fn device_type(&self) -> u32 {
        TYPE_RNG
    }

    fn queue_max_sizes(&self) -> Vec<u16> {
        vec![QUEUE_SIZE]
    }

    fn features(&self) -> u64 {
        0
    }

    fn activate(
        &mut self,
        mem: Mem,
        interrupt: Arc<Interrupt>,
        queues: Vec<ActiveQueue>,
    ) -> io::Result<()> {
        let mut queues = queues;
        let ActiveQueue { queue, kick, .. } = queues.remove(0);
        let mut worker = Worker {
            queue,
            mem,
            interrupt,
        };
        worker::spawn(
            "hangar-rng".to_string(),
            vec![kick],
            self.stop.clone(),
            move |_| worker.process(),
        )
    }
}

struct Worker {
    queue: virtio_queue::Queue,
    mem: Mem,
    interrupt: Arc<Interrupt>,
}

impl Worker {
    fn process(&mut self) {
        let Self {
            queue,
            mem,
            interrupt,
        } = self;
        worker::drain("rng", queue, mem, interrupt, |guard, chain| {
            let mut written = 0u32;
            for desc in chain {
                if !desc.is_write_only() {
                    continue;
                }
                let len = (desc.len() as usize).min(MAX_REQUEST);
                let mut buf = vec![0u8; len];
                if let Err(e) = fill(&mut buf) {
                    log::error!("rng: reading from the host: {e}");
                    break;
                }
                match guard.write_slice(&buf, desc.addr()) {
                    Ok(()) => written += len as u32,
                    Err(e) => {
                        log::error!("rng: writing to guest memory: {e}");
                        break;
                    }
                }
            }
            written
        });
    }
}

/// Fill `buf` from the host's random number generator.
fn fill(buf: &mut [u8]) -> io::Result<()> {
    let mut done = 0;
    while done < buf.len() {
        // SAFETY: the call writes at most the remaining length of a buffer
        // this function owns.
        let n = unsafe {
            libc::getrandom(
                buf[done..].as_mut_ptr() as *mut libc::c_void,
                buf.len() - done,
                0,
            )
        };
        if n < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(e);
        }
        done += n as usize;
    }
    Ok(())
}
