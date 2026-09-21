//! virtio-fs, served in this process, with a DAX window.
//!
//! Every other stack puts the filesystem in a separate daemon and talks to it
//! over vhost-user. That is why nobody has DAX: the FUSE `setupmapping`
//! request has to become a vhost-user message, the VMM has to honour it by
//! mapping a file descriptor into a window it exposes to the guest, and the
//! two halves were never finished at the same time -- virtiofsd answers
//! `setupmapping` with ENOSYS, and upstream QEMU has no window to map into.
//!
//! Here the filesystem is a library call away from the memory it needs to
//! modify, so `setupmapping` is an `mmap` into the window and nothing crosses
//! a process boundary. The guest then reads file contents by touching memory:
//! no FUSE round trip, no copy, and one host page cache shared by every guest
//! running the same base image.

use std::io;
use std::ops::Deref;
use std::os::unix::io::RawFd;
use std::sync::Arc;

use fuse_backend_rs::abi::virtio_fs::RemovemappingOne;
use fuse_backend_rs::api::server::Server;
use fuse_backend_rs::passthrough::{CachePolicy, Config as PassthroughConfig, PassthroughFs};
use fuse_backend_rs::transport::{FsCacheReqHandler, Reader, VirtioFsWriter, Writer};
use virtio_queue::QueueT;
use vm_memory::GuestAddressSpace;

use super::worker::{self, Stop};
use super::{fsopts, ActiveQueue, Interrupt, ShmRegion, VirtioDevice, TYPE_FS};
use crate::memory::{DaxWindow, Mem};

/// Tags are a fixed-width field in the configuration space.
const TAG_LEN: usize = 36;
const QUEUE_SIZE: u16 = 1024;

/// The shared memory region id the guest looks for when it asks whether there
/// is a DAX window, fixed by the virtio specification.
const SHM_ID_CACHE: u8 = 0;

/// Files at least this large are mapped rather than read.
///
/// Mapping has a fixed cost -- a FUSE round trip and an `mmap` -- that only
/// pays for itself once the file is big enough to be read more than once or
/// in more than one piece. Below it, a plain read is cheaper. The guest's
/// mapping granularity is 2 MiB, so anything smaller than one page is
/// certainly not worth a mapping.
const DAX_MIN_FILE_SIZE: u64 = 4096;

pub struct Fs {
    server: Arc<Server<fsopts::Fs>>,
    tag: String,
    request_queues: u16,
    dax: Option<Arc<DaxWindow>>,
    stop: Stop,
}

impl Fs {
    /// Export `shared_dir`, optionally through a DAX window.
    pub fn new(
        shared_dir: &std::path::Path,
        tag: String,
        request_queues: u16,
        dax: Option<Arc<DaxWindow>>,
        stop: Stop,
    ) -> io::Result<Self> {
        if tag.len() > TAG_LEN {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("the tag {tag:?} is longer than {TAG_LEN} bytes"),
            ));
        }

        let cfg = PassthroughConfig {
            root_dir: shared_dir.to_string_lossy().into_owned(),
            // The host is the only writer of a base layer, so nothing the
            // guest caches can go stale and there is nothing to revalidate.
            cache_policy: CachePolicy::Always,
            writeback: true,
            xattr: true,
            do_import: true,
            // Reuse host inode numbers where they fit, which keeps the
            // inode map small for a base image of many thousands of files.
            use_host_ino: true,
            dax_file_size: dax.as_ref().map(|_| DAX_MIN_FILE_SIZE),
            ..Default::default()
        };
        let fs = PassthroughFs::<()>::new(cfg)
            .map_err(|e| io::Error::other(format!("opening {}: {e}", shared_dir.display())))?;
        fs.import()
            .map_err(|e| io::Error::other(format!("importing {}: {e}", shared_dir.display())))?;

        Ok(Self {
            server: Arc::new(Server::new(fsopts::Fs::new(fs))),
            tag,
            request_queues,
            dax,
            stop,
        })
    }
}

impl VirtioDevice for Fs {
    fn device_type(&self) -> u32 {
        TYPE_FS
    }

    fn queue_max_sizes(&self) -> Vec<u16> {
        // Queue zero is the high priority queue, which carries FORGET and
        // interrupt messages; the rest carry everything else.
        vec![QUEUE_SIZE; 1 + self.request_queues as usize]
    }

    fn features(&self) -> u64 {
        0
    }

    fn read_config(&self, offset: u64, data: &mut [u8]) {
        let mut config = [0u8; TAG_LEN + 4];
        let tag = self.tag.as_bytes();
        config[..tag.len()].copy_from_slice(tag);
        config[TAG_LEN..].copy_from_slice(&u32::from(self.request_queues).to_le_bytes());
        for (i, byte) in data.iter_mut().enumerate() {
            *byte = config.get(offset as usize + i).copied().unwrap_or(0);
        }
    }

    fn shm_region(&self) -> Option<ShmRegion> {
        self.dax.as_ref().map(|w| ShmRegion {
            id: SHM_ID_CACHE,
            addr: w.guest_addr(),
            size: w.size(),
        })
    }

    fn activate(
        &mut self,
        mem: Mem,
        interrupt: Arc<Interrupt>,
        queues: Vec<ActiveQueue>,
    ) -> io::Result<()> {
        // Each queue gets a thread, so several guest threads reading the
        // filesystem do not queue behind one another.
        for ActiveQueue { index, queue, kick } in queues {
            let mut worker = Worker {
                queue,
                mem: mem.clone(),
                interrupt: interrupt.clone(),
                server: self.server.clone(),
                cache: self.dax.clone().map(CacheHandler),
            };
            worker::spawn(
                format!("hangar-fs-{index}"),
                vec![kick],
                self.stop.clone(),
                move |_| worker.process(),
            )?;
        }
        Ok(())
    }
}

struct Worker {
    queue: virtio_queue::Queue,
    mem: Mem,
    interrupt: Arc<Interrupt>,
    server: Arc<Server<fsopts::Fs>>,
    cache: Option<CacheHandler>,
}

impl Worker {
    fn process(&mut self) {
        let guard = self.mem.memory();
        let mut used_any = false;
        while let Some(chain) = self.queue.pop_descriptor_chain(guard.clone()) {
            let head = chain.head_index();
            let written = self.serve(&guard, chain);
            if let Err(e) = self.queue.add_used(guard.deref(), head, written) {
                log::error!("fs: returning a descriptor chain: {e}");
                break;
            }
            used_any = true;
        }
        if used_any {
            if let Err(e) = self.interrupt.signal_if_wanted(&mut self.queue, &self.mem) {
                log::error!("fs: raising the interrupt: {e}");
            }
        }
    }

    fn serve(
        &mut self,
        guard: &vm_memory::GuestMemoryLoadGuard<vm_memory::GuestMemoryMmap<()>>,
        chain: virtio_queue::DescriptorChain<
            vm_memory::GuestMemoryLoadGuard<vm_memory::GuestMemoryMmap<()>>,
        >,
    ) -> u32 {
        let mem = guard.deref();
        let reader = match Reader::from_descriptor_chain(mem, chain.clone()) {
            Ok(r) => r,
            Err(e) => {
                log::error!("fs: reading a request: {e}");
                return 0;
            }
        };
        let writer = match VirtioFsWriter::new(mem, chain) {
            Ok(w) => w,
            Err(e) => {
                log::error!("fs: preparing a reply: {e}");
                return 0;
            }
        };

        // The cache handler is what turns a `setupmapping` request into a
        // mapping in the DAX window. Passing None leaves the filesystem to
        // answer ENOSYS, which is what the guest sees when DAX is off.
        let result = match self.cache.as_mut() {
            Some(cache) => self.server.handle_message(
                reader,
                Writer::VirtioFs(writer),
                Some(cache as &mut dyn FsCacheReqHandler),
                None,
            ),
            None => self
                .server
                .handle_message(reader, Writer::VirtioFs(writer), None, None),
        };

        match result {
            Ok(n) => n as u32,
            Err(e) => {
                log::error!("fs: serving a request: {e}");
                0
            }
        }
    }
}

/// Turns the filesystem's mapping requests into changes to the DAX window.
///
/// This is the whole of DAX on this side. `moffset` is an offset the guest
/// chose inside the window it read out of the shared memory registers, so
/// mapping is placing the host's file at the address the guest is already
/// expecting to find it.
struct CacheHandler(Arc<DaxWindow>);

impl FsCacheReqHandler for CacheHandler {
    fn map(
        &mut self,
        foffset: u64,
        moffset: u64,
        len: u64,
        flags: u64,
        fd: RawFd,
    ) -> io::Result<()> {
        use fuse_backend_rs::abi::virtio_fs::SetupmappingFlags;
        let writable =
            SetupmappingFlags::from_bits_truncate(flags).contains(SetupmappingFlags::WRITE);
        self.0.map(moffset, foffset, len, writable, fd)
    }

    fn unmap(&mut self, requests: Vec<RemovemappingOne>) -> io::Result<()> {
        for r in requests {
            self.0.unmap(r.moffset, r.len)?;
        }
        Ok(())
    }
}
