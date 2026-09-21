//! virtio-blk, backed by a file on the host.
//!
//! The guest's writable layer is a block device rather than a filesystem
//! export: it is private to one environment, so nothing is gained by the host
//! understanding its contents, and a block device is the one thing a guest can
//! put any filesystem on.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::ops::Deref;
use std::sync::{Arc, Mutex};

use virtio_queue::desc::split::Descriptor;
use virtio_queue::{DescriptorChain, QueueT};
use vm_memory::{Bytes, GuestAddressSpace, GuestMemoryLoadGuard, GuestMemoryMmap};

use super::worker::{self, Stop};
use super::{ActiveQueue, Interrupt, VirtioDevice, TYPE_BLOCK};
use crate::memory::Mem;

const SECTOR_SIZE: u64 = 512;
const QUEUE_SIZE: u16 = 256;

/// Feature bits.
const F_RO: u64 = 1 << 5;
const F_FLUSH: u64 = 1 << 9;

/// Request types, from the header's first field.
const T_IN: u32 = 0;
const T_OUT: u32 = 1;
const T_FLUSH: u32 = 4;
const T_GET_ID: u32 = 8;

/// Status byte values.
const S_OK: u8 = 0;
const S_IOERR: u8 = 1;
const S_UNSUPP: u8 = 2;

/// Length of the device id string the guest can ask for.
const ID_LEN: usize = 20;

pub struct Block {
    file: Arc<Mutex<File>>,
    capacity_sectors: u64,
    read_only: bool,
    id: String,
    stop: Stop,
}

impl Block {
    pub fn new(
        path: &std::path::Path,
        read_only: bool,
        id: String,
        stop: Stop,
    ) -> io::Result<Self> {
        let file = File::options()
            .read(true)
            .write(!read_only)
            .open(path)
            .map_err(|e| io::Error::other(format!("opening {}: {e}", path.display())))?;
        let len = file.metadata()?.len();
        Ok(Self {
            file: Arc::new(Mutex::new(file)),
            capacity_sectors: len / SECTOR_SIZE,
            read_only,
            id,
            stop,
        })
    }
}

impl VirtioDevice for Block {
    fn device_type(&self) -> u32 {
        TYPE_BLOCK
    }

    fn queue_max_sizes(&self) -> Vec<u16> {
        vec![QUEUE_SIZE]
    }

    fn features(&self) -> u64 {
        let mut f = F_FLUSH;
        if self.read_only {
            f |= F_RO;
        }
        f
    }

    fn read_config(&self, offset: u64, data: &mut [u8]) {
        // The configuration space starts with the capacity in 512-byte
        // sectors; nothing past it is offered.
        let capacity = self.capacity_sectors.to_le_bytes();
        for (i, byte) in data.iter_mut().enumerate() {
            let at = offset as usize + i;
            *byte = capacity.get(at).copied().unwrap_or(0);
        }
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
            file: self.file.clone(),
            capacity_sectors: self.capacity_sectors,
            read_only: self.read_only,
            id: self.id.clone(),
        };
        worker::spawn(
            "hangar-blk".to_string(),
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
    file: Arc<Mutex<File>>,
    capacity_sectors: u64,
    read_only: bool,
    id: String,
}

impl Worker {
    fn process(&mut self) {
        log::debug!("blk: kicked");
        let guard = self.mem.memory();
        let mut used_any = false;
        while let Some(chain) = self.queue.pop_descriptor_chain(guard.clone()) {
            let head = chain.head_index();
            let written = self.serve(&guard, chain);
            if let Err(e) = self.queue.add_used(guard.deref(), head, written) {
                log::error!("blk: returning a descriptor chain: {e}");
                break;
            }
            used_any = true;
        }
        if used_any {
            if let Err(e) = self.interrupt.signal_queue() {
                log::error!("blk: raising the interrupt: {e}");
            }
        }
    }

    /// Serve one request, returning how many bytes were written into the
    /// guest's buffers.
    ///
    /// A malformed chain is completed with an error status rather than
    /// dropped: a request the guest never gets an answer to hangs whatever
    /// issued it, which is a far worse failure than an I/O error.
    fn serve(
        &mut self,
        guard: &GuestMemoryLoadGuard<GuestMemoryMmap<()>>,
        chain: DescriptorChain<GuestMemoryLoadGuard<GuestMemoryMmap<()>>>,
    ) -> u32 {
        let descriptors: Vec<_> = chain.clone().collect();
        // Every request is a header, then data, then a one-byte status.
        if descriptors.len() < 2 {
            return 0;
        }
        let header = &descriptors[0];
        let status_desc = &descriptors[descriptors.len() - 1];
        if status_desc.len() < 1 || !status_desc.is_write_only() {
            return 0;
        }

        let (req_type, sector) = match read_header(guard, header) {
            Some(h) => h,
            None => {
                let _ = guard.write_slice(&[S_IOERR], status_desc.addr());
                return 1;
            }
        };

        let data = &descriptors[1..descriptors.len() - 1];
        let mut written = 0u32;
        let status = match req_type {
            T_IN => match self.read_into(guard, sector, data, &mut written) {
                Ok(()) => S_OK,
                Err(e) => {
                    log::warn!("blk: read at sector {sector}: {e}");
                    S_IOERR
                }
            },
            T_OUT if self.read_only => S_IOERR,
            T_OUT => match self.write_from(guard, sector, data) {
                Ok(()) => S_OK,
                Err(e) => {
                    log::warn!("blk: write at sector {sector}: {e}");
                    S_IOERR
                }
            },
            T_FLUSH => match self.file.lock().unwrap().sync_data() {
                Ok(()) => S_OK,
                Err(e) => {
                    log::warn!("blk: flush: {e}");
                    S_IOERR
                }
            },
            T_GET_ID => {
                let mut id = [0u8; ID_LEN];
                let bytes = self.id.as_bytes();
                let n = bytes.len().min(ID_LEN);
                id[..n].copy_from_slice(&bytes[..n]);
                for d in data {
                    if !d.is_write_only() {
                        continue;
                    }
                    let n = (d.len() as usize).min(ID_LEN);
                    if guard.write_slice(&id[..n], d.addr()).is_ok() {
                        written += n as u32;
                    }
                }
                S_OK
            }
            _ => S_UNSUPP,
        };

        if guard.write_slice(&[status], status_desc.addr()).is_ok() {
            written += 1;
        }
        written
    }

    fn read_into(
        &mut self,
        guard: &GuestMemoryLoadGuard<GuestMemoryMmap<()>>,
        sector: u64,
        data: &[Descriptor],
        written: &mut u32,
    ) -> io::Result<()> {
        let mut file = self.file.lock().unwrap();
        let mut offset = self.offset_of(sector, data)?;
        for d in data {
            if !d.is_write_only() {
                continue;
            }
            file.seek(SeekFrom::Start(offset))?;
            let mut buf = vec![0u8; d.len() as usize];
            file.read_exact(&mut buf)?;
            guard
                .write_slice(&buf, d.addr())
                .map_err(|e| io::Error::other(format!("writing to guest memory: {e}")))?;
            offset += u64::from(d.len());
            *written += d.len();
        }
        Ok(())
    }

    fn write_from(
        &mut self,
        guard: &GuestMemoryLoadGuard<GuestMemoryMmap<()>>,
        sector: u64,
        data: &[Descriptor],
    ) -> io::Result<()> {
        let mut file = self.file.lock().unwrap();
        let mut offset = self.offset_of(sector, data)?;
        for d in data {
            if d.is_write_only() {
                continue;
            }
            let mut buf = vec![0u8; d.len() as usize];
            guard
                .read_slice(&mut buf, d.addr())
                .map_err(|e| io::Error::other(format!("reading from guest memory: {e}")))?;
            file.seek(SeekFrom::Start(offset))?;
            file.write_all(&buf)?;
            offset += u64::from(d.len());
        }
        Ok(())
    }

    /// The byte offset a request starts at, refusing one that runs past the
    /// end of the backing file.
    fn offset_of(&self, sector: u64, data: &[Descriptor]) -> io::Result<u64> {
        let len: u64 = data.iter().map(|d| u64::from(d.len())).sum();
        let end_sector = sector + len.div_ceil(SECTOR_SIZE);
        if end_sector > self.capacity_sectors {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "a request for sectors {sector}..{end_sector} runs past the \
                     device's {} sectors",
                    self.capacity_sectors
                ),
            ));
        }
        Ok(sector * SECTOR_SIZE)
    }
}

/// Read a request header: type, a reserved word, then the starting sector.
fn read_header(
    guard: &GuestMemoryLoadGuard<GuestMemoryMmap<()>>,
    desc: &Descriptor,
) -> Option<(u32, u64)> {
    if desc.len() < 16 || desc.is_write_only() {
        return None;
    }
    let mut buf = [0u8; 16];
    guard.read_slice(&mut buf, desc.addr()).ok()?;
    let req_type = u32::from_le_bytes(buf[0..4].try_into().ok()?);
    let sector = u64::from_le_bytes(buf[8..16].try_into().ok()?);
    Some((req_type, sector))
}
