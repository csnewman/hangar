//! The loop a device's thread runs.
//!
//! Every device that serves virtqueues does the same thing: wait for the
//! guest to kick a queue, drain it, repeat, and stop when the VM does. That
//! loop lives here so the devices are only their protocol.

use std::io;
use std::sync::Arc;
use std::thread;

use vmm_sys_util::epoll::{ControlOperation, Epoll, EpollEvent, EventSet};
use vmm_sys_util::eventfd::EventFd;

/// Told to every device thread when the VM is going away.
#[derive(Clone)]
pub struct Stop(Arc<EventFd>);

impl Stop {
    pub fn new() -> io::Result<Self> {
        Ok(Self(Arc::new(EventFd::new(libc::EFD_NONBLOCK)?)))
    }

    /// Wake every thread waiting on this and tell it to finish.
    pub fn signal(&self) {
        let _ = self.0.write(1);
    }

    fn fd(&self) -> i32 {
        use std::os::unix::io::AsRawFd;
        self.0.as_raw_fd()
    }
}

/// The token an epoll event carries for the stop descriptor. Queue indices
/// use their own number, so this sits above any queue a device can have.
const STOP_TOKEN: u64 = u64::MAX;

/// Run `handle` on its own thread whenever the guest kicks a queue.
///
/// `handle` is given the index of the queue that was kicked. It is called
/// after the kick has been consumed, so a device that leaves work undone must
/// arrange to be called again rather than relying on the descriptor staying
/// readable.
pub fn spawn<F>(name: String, kicks: Vec<EventFd>, stop: Stop, mut handle: F) -> io::Result<()>
where
    F: FnMut(usize) + Send + 'static,
{
    use std::os::unix::io::AsRawFd;

    let epoll = Epoll::new()?;
    for (i, kick) in kicks.iter().enumerate() {
        epoll.ctl(
            ControlOperation::Add,
            kick.as_raw_fd(),
            EpollEvent::new(EventSet::IN, i as u64),
        )?;
    }
    epoll.ctl(
        ControlOperation::Add,
        stop.fd(),
        EpollEvent::new(EventSet::IN, STOP_TOKEN),
    )?;

    thread::Builder::new()
        .name(name.clone())
        .spawn(move || {
            let mut events = vec![EpollEvent::default(); kicks.len() + 1];
            loop {
                let n = match epoll.wait(-1, &mut events) {
                    Ok(n) => n,
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(e) => {
                        log::error!("{name}: waiting for events: {e}");
                        return;
                    }
                };
                for event in events.iter().take(n) {
                    let token = event.data();
                    if token == STOP_TOKEN {
                        return;
                    }
                    let index = token as usize;
                    // Consume the kick before doing the work, so a kick that
                    // arrives while the queue is being drained is not lost.
                    let _ = kicks[index].read();
                    handle(index);
                }
            }
        })
        .map(|_| ())
}
