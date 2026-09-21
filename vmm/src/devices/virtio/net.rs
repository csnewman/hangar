//! virtio-net, carried by passt.
//!
//! passt is an unprivileged user-mode network: it holds ordinary sockets on
//! the host and translates between them and Ethernet frames, so a guest gets
//! outbound connectivity with no TAP device, no bridge and no NET_ADMIN. In
//! Kubernetes that means the VM rides the pod's own networking rather than
//! needing a CNI of its own.
//!
//! The frames cross a socket pair rather than shared memory. passt can speak
//! vhost-user instead, which would be fewer copies, but vhost-user requires
//! the guest's RAM to be shared -- and shared memory does not get transparent
//! huge pages under the usual host policy, which costs far more than the
//! copies save. See `docs/vmm-rust.md`.

use std::io;
use std::ops::Deref;
use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command};
use std::sync::Arc;

use virtio_queue::{Queue, QueueT};
use vm_memory::{Bytes, GuestAddressSpace};
use vmm_sys_util::epoll::{ControlOperation, Epoll, EpollEvent, EventSet};
use vmm_sys_util::eventfd::EventFd;

use super::worker::Stop;
use super::{ActiveQueue, Interrupt, VirtioDevice, TYPE_NET};
use crate::memory::Mem;

const QUEUE_SIZE: u16 = 256;
/// Receive and transmit. No control queue: nothing here needs one, and not
/// offering it keeps the guest from asking for features it cannot have.
const NUM_QUEUES: usize = 2;
const RX: usize = 0;
const TX: usize = 1;

/// Feature bits.
const F_MTU: u64 = 1 << 3;
const F_MAC: u64 = 1 << 5;

/// Every frame carries this header. With VIRTIO_F_VERSION_1 negotiated the
/// guest always uses the twelve byte form, whether or not buffers merge.
const HDR_LEN: usize = 12;

/// passt frames each packet with its length, most significant byte first.
const LEN_PREFIX: usize = 4;

/// The largest frame either side will send, and so the size of one buffer.
const MAX_FRAME: usize = 65_535;

pub struct Net {
    mac: [u8; 6],
    mtu: u16,
    stop: Stop,
    /// passt, kept for the life of the VM. Dropping it takes the guest's
    /// network away.
    passt: Option<Child>,
    /// This process's end of the pair passt talks over.
    sock: Option<UnixStream>,
}

impl Net {
    /// Start passt and keep the end of the socket pair the device reads.
    pub fn new(mac: [u8; 6], mtu: u16, stop: Stop) -> io::Result<Self> {
        let (ours, theirs) = UnixStream::pair()?;

        // passt is handed an already connected socket, so there is no path to
        // agree on, nothing to clean up, and no window where something else
        // could connect to it.
        let their_fd = OwnedFd::from(theirs);
        let raw = their_fd.as_raw_fd();
        clear_cloexec(raw)?;

        let mut cmd = Command::new("passt");
        cmd.args([
            "--foreground",
            "--quiet",
            "--fd",
            &raw.to_string(),
            "--mtu",
            &mtu.to_string(),
        ]);
        // Tie passt's life to this process's. Dropping the device kills it,
        // but a monitor that is killed outright never gets to drop anything,
        // and a network left running for a guest that is gone is both a leak
        // and a way for the next guest to find a socket it did not open.
        //
        // SAFETY: between fork and exec only async-signal-safe calls are
        // made. The getppid check closes the window where the parent dies
        // before prctl runs, which would otherwise leave the signal armed
        // against a parent that no longer exists.
        unsafe {
            cmd.pre_exec(|| {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) != 0 {
                    return Err(io::Error::last_os_error());
                }
                if libc::getppid() == 1 {
                    libc::_exit(1);
                }
                Ok(())
            });
        }
        let passt = cmd
            .spawn()
            .map_err(|e| io::Error::other(format!("starting passt (apt install passt): {e}")))?;
        drop(their_fd);

        ours.set_nonblocking(true)?;
        Ok(Self {
            mac,
            mtu,
            stop,
            passt: Some(passt),
            sock: Some(ours),
        })
    }
}

impl Drop for Net {
    fn drop(&mut self) {
        if let Some(mut p) = self.passt.take() {
            let _ = p.kill();
            let _ = p.wait();
        }
    }
}

impl VirtioDevice for Net {
    fn device_type(&self) -> u32 {
        TYPE_NET
    }

    fn queue_max_sizes(&self) -> Vec<u16> {
        vec![QUEUE_SIZE; NUM_QUEUES]
    }

    fn features(&self) -> u64 {
        F_MAC | F_MTU
    }

    fn read_config(&self, offset: u64, data: &mut [u8]) {
        // mac[6], then status, then the queue pair count, then the MTU.
        let mut config = [0u8; 12];
        config[..6].copy_from_slice(&self.mac);
        config[6..8].copy_from_slice(&1u16.to_le_bytes()); // link up
        config[8..10].copy_from_slice(&1u16.to_le_bytes()); // one queue pair
        config[10..12].copy_from_slice(&self.mtu.to_le_bytes());
        for (i, byte) in data.iter_mut().enumerate() {
            *byte = config.get(offset as usize + i).copied().unwrap_or(0);
        }
    }

    fn activate(
        &mut self,
        mem: Mem,
        interrupt: Arc<Interrupt>,
        queues: Vec<ActiveQueue>,
    ) -> io::Result<()> {
        if queues.len() < NUM_QUEUES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "net needs {NUM_QUEUES} queues, the driver readied {}",
                    queues.len()
                ),
            ));
        }
        let sock = self
            .sock
            .take()
            .ok_or_else(|| io::Error::other("the network device was activated twice"))?;

        let mut queues = queues;
        queues.sort_by_key(|q| q.index);
        let tx = queues.remove(TX);
        let rx = queues.remove(RX);

        let worker = Worker {
            rx: rx.queue,
            tx: tx.queue,
            rx_kick: rx.kick,
            tx_kick: tx.kick,
            sock,
            mem,
            interrupt,
            stop: self.stop.clone(),
            pending: Vec::new(),
            incoming: Vec::with_capacity(MAX_FRAME + LEN_PREFIX),
        };
        worker.spawn()
    }
}

/// Tokens the one network thread waits on.
const TOKEN_RX: u64 = 0;
const TOKEN_TX: u64 = 1;
const TOKEN_SOCK: u64 = 2;
const TOKEN_STOP: u64 = 3;

struct Worker {
    rx: Queue,
    tx: Queue,
    rx_kick: EventFd,
    tx_kick: EventFd,
    sock: UnixStream,
    mem: Mem,
    interrupt: Arc<Interrupt>,
    stop: Stop,
    /// A frame read from passt that no receive buffer was free for.
    ///
    /// Holding it, and not reading further, is what applies backpressure: the
    /// alternative is to drop frames the guest has room for a moment later.
    pending: Vec<u8>,
    /// Bytes read from passt that do not yet make a whole frame.
    incoming: Vec<u8>,
}

impl Worker {
    fn spawn(mut self) -> io::Result<()> {
        let epoll = Epoll::new()?;
        epoll.ctl(
            ControlOperation::Add,
            self.rx_kick.as_raw_fd(),
            EpollEvent::new(EventSet::IN, TOKEN_RX),
        )?;
        epoll.ctl(
            ControlOperation::Add,
            self.tx_kick.as_raw_fd(),
            EpollEvent::new(EventSet::IN, TOKEN_TX),
        )?;
        epoll.ctl(
            ControlOperation::Add,
            self.sock.as_raw_fd(),
            EpollEvent::new(EventSet::IN, TOKEN_SOCK),
        )?;
        epoll.ctl(
            ControlOperation::Add,
            self.stop.raw_fd(),
            EpollEvent::new(EventSet::IN, TOKEN_STOP),
        )?;

        std::thread::Builder::new()
            .name("hangar-net".to_string())
            .spawn(move || {
                let mut events = vec![EpollEvent::default(); 4];
                loop {
                    let n = match epoll.wait(-1, &mut events) {
                        Ok(n) => n,
                        Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                        Err(e) => {
                            log::error!("net: waiting for events: {e}");
                            return;
                        }
                    };
                    for event in events.iter().take(n) {
                        match event.data() {
                            TOKEN_STOP => return,
                            TOKEN_TX => {
                                let _ = self.tx_kick.read();
                                self.transmit();
                            }
                            TOKEN_RX => {
                                let _ = self.rx_kick.read();
                                self.receive();
                            }
                            TOKEN_SOCK => self.receive(),
                            other => log::error!("net: unexpected event {other}"),
                        }
                    }
                }
            })
            .map(|_| ())
    }

    /// Send everything the guest has queued.
    ///
    /// The queue is silenced while it is drained and re-armed afterwards,
    /// which is also what publishes the point the guest should kick from. A
    /// device that never re-arms is a device the guest stops kicking.
    fn transmit(&mut self) {
        let guard = self.mem.memory();
        let mut frame = Vec::with_capacity(MAX_FRAME);
        loop {
            if let Err(e) = self.tx.disable_notification(guard.deref()) {
                log::error!("net: silencing the transmit queue: {e}");
                return;
            }
            let mut used_any = false;
            while let Some(chain) = self.tx.pop_descriptor_chain(guard.clone()) {
                let head = chain.head_index();
                frame.clear();
                for desc in chain.readable() {
                    let len = desc.len() as usize;
                    let at = frame.len();
                    frame.resize(at + len, 0);
                    if guard.read_slice(&mut frame[at..], desc.addr()).is_err() {
                        frame.truncate(at);
                        break;
                    }
                }
                // The guest puts its own header in front of the frame; passt
                // wants the Ethernet frame alone.
                if frame.len() > HDR_LEN {
                    if let Err(e) = self.write_frame(&frame[HDR_LEN..]) {
                        log::warn!("net: sending a frame: {e}");
                    }
                }
                if let Err(e) = self.tx.add_used(guard.deref(), head, 0) {
                    log::error!("net: returning a transmit buffer: {e}");
                    return;
                }
                used_any = true;
            }
            if used_any {
                if let Err(e) = self.interrupt.signal_if_wanted(&mut self.tx, &self.mem) {
                    log::error!("net: raising the interrupt: {e}");
                }
            }
            match self.tx.enable_notification(guard.deref()) {
                Ok(true) => continue,
                Ok(false) => return,
                Err(e) => {
                    log::error!("net: re-arming the transmit queue: {e}");
                    return;
                }
            }
        }
    }

    /// Give the guest everything passt has for it, while it has room.
    ///
    /// When the guest has no buffer free the frame stays pending and the
    /// receive queue is re-armed, so the guest's next refill is announced.
    /// Without that the frame waits for a kick the guest has decided not to
    /// send, and the connection silently stops -- which is what a DHCP reply
    /// arriving a moment too early looks like.
    fn receive(&mut self) {
        let mut used_any = false;
        loop {
            if self.pending.is_empty() && !self.read_frame() {
                break;
            }
            let guard = self.mem.memory();
            let chain = match self.rx.pop_descriptor_chain(guard.clone()) {
                Some(c) => c,
                None => {
                    // Announce that a buffer is wanted, then look once more:
                    // one may have arrived while the queue was silent.
                    match self.rx.enable_notification(guard.deref()) {
                        Ok(true) => continue,
                        Ok(false) => break,
                        Err(e) => {
                            log::error!("net: re-arming the receive queue: {e}");
                            break;
                        }
                    }
                }
            };
            let head = chain.head_index();
            let written = match write_into(&guard, chain, &self.pending) {
                Ok(n) => n,
                Err(e) => {
                    log::warn!("net: delivering a frame: {e}");
                    0
                }
            };
            self.pending.clear();
            if let Err(e) = self.rx.add_used(guard.deref(), head, written) {
                log::error!("net: returning a receive buffer: {e}");
                break;
            }
            used_any = true;
        }
        if used_any {
            if let Err(e) = self.interrupt.signal_if_wanted(&mut self.rx, &self.mem) {
                log::error!("net: raising the interrupt: {e}");
            }
        }
    }

    /// Move one whole frame from passt into `pending`.
    ///
    /// Returns false when there is not a whole frame to be had, which is the
    /// signal to stop and wait for the socket to become readable again.
    fn read_frame(&mut self) -> bool {
        loop {
            if self.incoming.len() >= LEN_PREFIX {
                let len =
                    u32::from_be_bytes(self.incoming[..LEN_PREFIX].try_into().unwrap()) as usize;
                if len > MAX_FRAME {
                    log::error!("net: passt announced a {len} byte frame; closing");
                    self.incoming.clear();
                    return false;
                }
                if self.incoming.len() >= LEN_PREFIX + len {
                    self.pending
                        .extend_from_slice(&self.incoming[LEN_PREFIX..LEN_PREFIX + len]);
                    self.incoming.drain(..LEN_PREFIX + len);
                    return true;
                }
            }

            let mut buf = [0u8; 16 * 1024];
            match io::Read::read(&mut self.sock, &mut buf) {
                Ok(0) => {
                    log::error!("net: passt closed the connection");
                    return false;
                }
                Ok(n) => self.incoming.extend_from_slice(&buf[..n]),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return false,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    log::error!("net: reading from passt: {e}");
                    return false;
                }
            }
        }
    }

    /// Write one frame to passt, length first.
    fn write_frame(&mut self, frame: &[u8]) -> io::Result<()> {
        let mut buf = Vec::with_capacity(LEN_PREFIX + frame.len());
        buf.extend_from_slice(&(frame.len() as u32).to_be_bytes());
        buf.extend_from_slice(frame);

        let mut sent = 0;
        while sent < buf.len() {
            match io::Write::write(&mut self.sock, &buf[sent..]) {
                Ok(0) => return Err(io::Error::other("passt stopped accepting frames")),
                Ok(n) => sent += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    // A frame already half written cannot be abandoned: passt
                    // would read the remainder as a length. Wait for room.
                    wait_writable(self.sock.as_raw_fd())?;
                }
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
}

/// Put the virtio header and `frame` into the guest's buffers.
fn write_into(
    guard: &super::worker::Guard,
    chain: virtio_queue::DescriptorChain<super::worker::Guard>,
    frame: &[u8],
) -> io::Result<u32> {
    // An all-zero header says: no checksum offload, no segmentation, one
    // buffer.
    let mut header = [0u8; HDR_LEN];
    header[10..12].copy_from_slice(&1u16.to_le_bytes());

    let mut written = 0usize;
    let mut source = header.iter().copied().chain(frame.iter().copied());
    let total = HDR_LEN + frame.len();

    for desc in chain.writable() {
        if written >= total {
            break;
        }
        let want = (desc.len() as usize).min(total - written);
        let bytes: Vec<u8> = source.by_ref().take(want).collect();
        guard
            .write_slice(&bytes, desc.addr())
            .map_err(|e| io::Error::other(format!("writing to guest memory: {e}")))?;
        written += bytes.len();
    }
    if written < total {
        return Err(io::Error::other(format!(
            "a {total} byte frame did not fit in the guest's buffers"
        )));
    }
    Ok(written as u32)
}

/// Wait until `fd` will accept more bytes.
fn wait_writable(fd: RawFd) -> io::Result<()> {
    let mut p = libc::pollfd {
        fd,
        events: libc::POLLOUT,
        revents: 0,
    };
    // SAFETY: one descriptor, owned by the caller, for the length of the call.
    let n = unsafe { libc::poll(&mut p, 1, 1000) };
    if n < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// passt inherits this descriptor, so it must survive the exec.
fn clear_cloexec(fd: RawFd) -> io::Result<()> {
    // SAFETY: the descriptor is open and owned by the caller.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: as above.
    if unsafe { libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Silence the unused warning for a type only named in a signature.
#[allow(dead_code)]
fn _assert_fd(_: OwnedFd) {}

/// Keeps the raw-fd conversion visible where passt's descriptor is made.
#[allow(dead_code)]
unsafe fn _from_raw(fd: RawFd) -> UnixStream {
    unsafe { UnixStream::from_raw_fd(fd) }
}
