//! FUSE over io_uring (Linux 6.14+).
//!
//! Each CPU has a queue in the kernel, and a request goes to the queue of the
//! CPU its caller is running on. Here each queue is a ring of its own, served
//! by a thread pinned to that CPU, so a request is answered without crossing
//! to another CPU or going through `read` and `writev` on `/dev/fuse`.
//!
//! A ring entry is two buffers registered with the kernel: a header area
//! (`struct fuse_uring_req_header`) and a payload. A request arrives as its
//! `fuse_in_header` and its operation's own header in the header area, the
//! rest of its arguments in the payload, and the entry's commit ID, which is
//! the request's unique ID, beside them. The reply goes back the same way: the
//! `fuse_out_header` in the header area, its data in the payload, and one
//! `COMMIT_AND_FETCH` command both commits it and gives the entry back for
//! the next request.
//!
//! fuser parses a request from one contiguous buffer, so the payload is
//! registered a fixed distance into a larger one, and the two headers are
//! copied in front of it: the arguments are not moved.

use std::io;
use std::io::IoSlice;
use std::os::fd::AsFd;
use std::os::fd::AsRawFd;
use std::sync::Arc;
use std::time::Duration;

use io_uring::IoUring;
use io_uring::cqueue;
use io_uring::opcode;
use io_uring::squeue;
use io_uring::types;
use log::error;
use log::info;
use log::warn;
use parking_lot::Mutex;

use crate::Filesystem;
use crate::ReplyEmpty;
use crate::ll::Operation;
use crate::request::RequestWithSender;
use crate::session::MAX_WRITE_SIZE;
use crate::session::SessionEventLoop;

const CMD_REGISTER: u32 = 1;
const CMD_COMMIT_AND_FETCH: u32 = 2;

/// `struct fuse_uring_req_header`: `in_out` (the in or out header) at 0,
/// `op_in` at 128, and `ring_ent_in_out` at 256, whose `commit_id` is at 8
/// and `payload_sz` at 16.
const HEADERS_SIZE: usize = 288;
const OP_IN: usize = 128;
const OP_IN_MAX: usize = 128;
const COMMIT_ID: usize = 256 + 8;
const PAYLOAD_SZ: usize = 256 + 16;

const IN_HEADER: usize = 40;
const OUT_HEADER: usize = 16;

/// Where the payload starts in an entry's buffer: room for the in header and
/// the largest operation header in front of it.
const PREFIX: usize = IN_HEADER + OP_IN_MAX;

/// The payload's size. The kernel's is the larger of `max_write` and
/// `max_pages` pages, both at most this, and it refuses a smaller buffer.
/// Only what a request fills is ever touched.
const PAYLOAD: usize = MAX_WRITE_SIZE;

/// What one entry is handling: the reply it has been given, and which
/// request it is for, so a reply that comes after its request has been
/// committed is refused rather than sent with another.
#[derive(Debug, Default)]
struct SlotState {
    generation: u64,
    reply: Option<Vec<u8>>,
}

/// Where a request's reply goes when it came on a ring entry. The entry's
/// thread commits it once the filesystem's method returns.
#[derive(Debug, Clone)]
pub(crate) struct RingReply {
    slot: Arc<Mutex<SlotState>>,
    generation: u64,
}

impl RingReply {
    pub(crate) fn send(&self, data: &[IoSlice<'_>]) -> io::Result<()> {
        let mut slot = self.slot.lock();
        if slot.generation != self.generation || slot.reply.is_some() {
            return Err(io::Error::other(
                "reply to a ring request after it was committed",
            ));
        }
        let mut reply = Vec::with_capacity(data.iter().map(|d| d.len()).sum());
        for d in data {
            reply.extend_from_slice(d);
        }
        slot.reply = Some(reply);
        Ok(())
    }
}

/// One ring entry's buffers, at fixed addresses the kernel holds.
struct Entry {
    headers: Vec<u64>,
    buf: Vec<u64>,
    iov: Box<[libc::iovec; 2]>,
    slot: Arc<Mutex<SlotState>>,
}

impl Entry {
    fn new() -> Entry {
        let mut headers = vec![0u64; HEADERS_SIZE / 8];
        let mut buf = vec![0u64; (PREFIX + PAYLOAD) / 8];
        let iov = Box::new([
            libc::iovec {
                iov_base: headers.as_mut_ptr().cast(),
                iov_len: HEADERS_SIZE,
            },
            libc::iovec {
                // SAFETY: PREFIX is within buf.
                iov_base: unsafe { buf.as_mut_ptr().cast::<u8>().add(PREFIX) }.cast(),
                iov_len: PAYLOAD,
            },
        ]);
        Entry {
            headers,
            buf,
            iov,
            slot: Arc::default(),
        }
    }

    fn headers(&mut self) -> *mut u8 {
        self.headers.as_mut_ptr().cast()
    }

    fn buf(&mut self) -> *mut u8 {
        self.buf.as_mut_ptr().cast()
    }

    fn read_u32(&mut self, at: usize) -> u32 {
        // SAFETY: every offset read is within the header area.
        unsafe { self.headers().add(at).cast::<u32>().read_unaligned() }
    }

    fn read_u64(&mut self, at: usize) -> u64 {
        // SAFETY: as read_u32.
        unsafe { self.headers().add(at).cast::<u64>().read_unaligned() }
    }

    /// Lays the request out contiguously in front of its payload, and
    /// returns where it starts and its length, or None if the kernel's
    /// lengths do not add up.
    fn assemble(&mut self) -> Option<(usize, usize)> {
        let len = self.read_u32(0) as usize;
        let payload = self.read_u32(PAYLOAD_SZ) as usize;
        let op = len.checked_sub(IN_HEADER + payload)?;
        if op > OP_IN_MAX || payload > PAYLOAD {
            return None;
        }
        let start = PREFIX - IN_HEADER - op;
        let (headers, buf) = (self.headers(), self.buf());
        // SAFETY: both copies stay within the header area and the prefix.
        unsafe {
            std::ptr::copy_nonoverlapping(headers, buf.add(start), IN_HEADER);
            std::ptr::copy_nonoverlapping(headers.add(OP_IN), buf.add(start + IN_HEADER), op);
        }
        Some((start, len))
    }

    /// Puts a reply, `fuse_out_header` then data, where the kernel reads it.
    fn place_reply(&mut self, reply: &[u8]) {
        let data = &reply[OUT_HEADER..];
        let (headers, buf) = (self.headers(), self.buf());
        // SAFETY: the out header fits the header area, and data was checked
        // against the payload's size.
        unsafe {
            std::ptr::copy_nonoverlapping(reply.as_ptr(), headers, OUT_HEADER);
            std::ptr::copy_nonoverlapping(data.as_ptr(), buf.add(PREFIX), data.len());
            headers
                .add(PAYLOAD_SZ)
                .cast::<u32>()
                .write_unaligned(data.len() as u32);
        }
    }
}

/// A `fuse_out_header` alone, for an error.
fn error_reply(unique: u64, errno: i32) -> Vec<u8> {
    let mut b = Vec::with_capacity(OUT_HEADER);
    b.extend_from_slice(&(OUT_HEADER as u32).to_ne_bytes());
    b.extend_from_slice(&(-errno).to_ne_bytes());
    b.extend_from_slice(&unique.to_ne_bytes());
    b
}

fn command(
    fd: i32,
    op: u32,
    qid: u16,
    commit_id: u64,
    iov: Option<&[libc::iovec; 2]>,
    user_data: u64,
) -> squeue::Entry128 {
    // struct fuse_uring_cmd_req: flags, commit_id, qid.
    let mut cmd = [0u8; 80];
    cmd[8..16].copy_from_slice(&commit_id.to_ne_bytes());
    cmd[16..18].copy_from_slice(&qid.to_ne_bytes());
    let mut b = opcode::UringCmd80::new(types::Fd(fd), op).cmd(cmd);
    if let Some(iov) = iov {
        b = b.addr(Some(iov.as_ptr() as u64));
    }
    let e = b.build().user_data(user_data);
    if iov.is_none() {
        return e;
    }
    // The iovec array's length, which UringCmd80 has no setter for:
    // io_uring_sqe.len, at 24.
    // SAFETY: Entry128 is repr(C), an io_uring_sqe and its 64 more bytes.
    let mut raw: [u8; 128] = unsafe { std::mem::transmute(e) };
    raw[24..28].copy_from_slice(&2u32.to_ne_bytes());
    unsafe { std::mem::transmute(raw) }
}

/// The number of CPUs the kernel can ever run: it has a queue for each, and
/// uses none until all of them are registered.
pub(crate) fn queues() -> usize {
    let parsed = std::fs::read_to_string("/sys/devices/system/cpu/possible")
        .ok()
        .and_then(|s| {
            let mut n = 0;
            for r in s.trim().split(',') {
                n += match r.split_once('-') {
                    Some((a, b)) => b.parse::<usize>().ok()? - a.parse::<usize>().ok()? + 1,
                    None => {
                        r.parse::<usize>().ok()?;
                        1
                    }
                };
            }
            Some(n)
        });
    parsed.unwrap_or_else(|| std::thread::available_parallelism().map_or(1, |n| n.get()))
}

fn pin(cpu: usize) {
    // SAFETY: a zeroed cpu_set_t is empty, and CPU_SET bounds-checks.
    unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        libc::CPU_SET(cpu, &mut set);
        if libc::sched_setaffinity(0, size_of::<libc::cpu_set_t>(), &set) != 0 {
            warn!(
                "ring {cpu}: pinning to its CPU: {}",
                io::Error::last_os_error()
            );
        }
    }
}

fn push(ring: &mut IoUring<squeue::Entry128, cqueue::Entry>, e: &squeue::Entry128) -> io::Result<()> {
    loop {
        // SAFETY: every buffer an entry names outlives the ring.
        if unsafe { ring.submission().push(e) }.is_ok() {
            return Ok(());
        }
        ring.submit()?;
    }
}

/// Serves queue `qid` with `n` entries until the connection ends. It returns
/// without error if the kernel refuses io_uring, leaving the `/dev/fuse`
/// threads to serve everything.
pub(crate) fn serve<FS: Filesystem>(se: &SessionEventLoop<FS>, qid: u16, n: usize) -> io::Result<()> {
    pin(qid.into());
    let fd = se.ch.as_fd().as_raw_fd();
    // Declared before the ring, so the ring, and the kernel's hold on these
    // buffers, goes first.
    let mut entries: Vec<Entry> = (0..n).map(|_| Entry::new()).collect();
    let depth = (2 * n).next_power_of_two() as u32;
    let mut ring = IoUring::<squeue::Entry128, cqueue::Entry>::builder()
        .setup_single_issuer()
        .setup_defer_taskrun()
        .build(depth)
        .or_else(|_| IoUring::<squeue::Entry128, cqueue::Entry>::builder().build(depth))?;

    for (i, e) in entries.iter().enumerate() {
        push(&mut ring, &command(fd, CMD_REGISTER, qid, 0, Some(&e.iov), i as u64))?;
    }
    let mut done = Vec::with_capacity(n);
    loop {
        match ring.submit_and_wait(1) {
            Ok(_) => {}
            Err(e) if e.raw_os_error() == Some(libc::EINTR) => continue,
            Err(e) => return Err(e),
        }
        done.clear();
        done.extend(ring.completion().map(|c| (c.user_data() as usize, c.result())));
        for &(i, res) in &done {
            let e = &mut entries[i];
            match -res {
                0 => {}
                // Registered before the kernel finished processing INIT.
                libc::EAGAIN => {
                    std::thread::sleep(Duration::from_millis(5));
                    push(&mut ring, &command(fd, CMD_REGISTER, qid, 0, Some(&e.iov), i as u64))?;
                    continue;
                }
                libc::EOPNOTSUPP => {
                    if qid == 0 {
                        info!("FUSE over io_uring is not enabled; serving /dev/fuse alone");
                    }
                    return Ok(());
                }
                // The connection ended.
                libc::ENOTCONN | libc::ECONNABORTED | libc::ENODEV | libc::ECANCELED => {
                    return Ok(());
                }
                err => return Err(io::Error::from_raw_os_error(err)),
            }
            let commit_id = e.read_u64(COMMIT_ID);
            let reply = handle(se, e, commit_id);
            let reply = if reply.len() - OUT_HEADER > PAYLOAD {
                error!("reply to {commit_id} is larger than the ring's payload");
                error_reply(commit_id, libc::EIO)
            } else {
                reply
            };
            e.place_reply(&reply);
            push(&mut ring, &command(fd, CMD_COMMIT_AND_FETCH, qid, commit_id, None, i as u64))?;
        }
    }
}

/// Dispatches the request in an entry, and returns its reply.
fn handle<FS: Filesystem>(se: &SessionEventLoop<FS>, e: &mut Entry, commit_id: u64) -> Vec<u8> {
    let generation = {
        let mut slot = e.slot.lock();
        slot.generation += 1;
        slot.reply = None;
        slot.generation
    };
    let ring = RingReply {
        slot: e.slot.clone(),
        generation,
    };
    match e.assemble() {
        Some((start, len)) => {
            // SAFETY: assemble laid out len bytes from start within buf,
            // which the kernel does not touch until the commit.
            let data = unsafe { std::slice::from_raw_parts(e.buf().add(start), len) };
            match RequestWithSender::new(se.ch.sender(), data) {
                Some(req) => {
                    let req = req.with_ring(ring);
                    if let Ok(Operation::Destroy(_)) = req.request.operation() {
                        req.reply::<ReplyEmpty>().ok();
                    } else {
                        req.dispatch(se);
                    }
                }
                None => error!("ring {}: invalid request {commit_id}", se.thread_name),
            }
        }
        None => error!("ring {}: request {commit_id} has inconsistent lengths", se.thread_name),
    }
    let mut slot = e.slot.lock();
    slot.generation += 1;
    slot.reply.take().unwrap_or_else(|| {
        error!("ring {}: no reply to {commit_id} by return", se.thread_name);
        error_reply(commit_id, libc::EIO)
    })
}
