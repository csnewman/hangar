//! Whether this Mac's network costs its owner: expensive (Personal
//! Hotspot, cellular) or constrained (Low Data Mode), as Network.framework's
//! path monitor says. Relaying through the app pauses while it does, unless
//! the person says otherwise.

use std::sync::Once;
use std::sync::atomic::{AtomicBool, Ordering};

static EXPENSIVE: AtomicBool = AtomicBool::new(false);
static START: Once = Once::new();

/// Whether the network is expensive or constrained now. The first call
/// starts watching it; until the monitor first answers, it is not.
pub fn expensive() -> bool {
    START.call_once(watch);
    EXPENSIVE.load(Ordering::Relaxed)
}

#[cfg(target_os = "macos")]
fn watch() {
    mac::start();
}

#[cfg(not(target_os = "macos"))]
fn watch() {}

#[cfg(target_os = "macos")]
mod mac {
    //! nw_path_monitor's update handler is a block. One that captures
    //! nothing is a global block: a fixed struct with no copy or dispose
    //! helpers, which Block_copy hands back as it is, so it is made here
    //! by hand rather than through a blocks crate.

    use std::ffi::c_void;
    use std::sync::atomic::Ordering;

    type NwPath = *mut c_void;

    #[repr(C)]
    struct Descriptor {
        reserved: usize,
        size: usize,
    }

    #[repr(C)]
    struct Block {
        isa: *const c_void,
        flags: i32,
        reserved: i32,
        invoke: extern "C" fn(*const Block, NwPath),
        descriptor: *const Descriptor,
    }

    const BLOCK_IS_GLOBAL: i32 = 1 << 28;

    unsafe extern "C" {
        static _NSConcreteGlobalBlock: c_void;
        fn dispatch_get_global_queue(identifier: isize, flags: usize) -> *mut c_void;
    }

    #[link(name = "Network", kind = "framework")]
    unsafe extern "C" {
        fn nw_path_monitor_create() -> *mut c_void;
        fn nw_path_monitor_set_queue(monitor: *mut c_void, queue: *mut c_void);
        fn nw_path_monitor_set_update_handler(monitor: *mut c_void, handler: *const Block);
        fn nw_path_monitor_start(monitor: *mut c_void);
        fn nw_path_is_expensive(path: NwPath) -> bool;
        fn nw_path_is_constrained(path: NwPath) -> bool;
    }

    extern "C" fn update(_: *const Block, path: NwPath) {
        // SAFETY: the monitor hands the handler a live path.
        let costly = unsafe { nw_path_is_expensive(path) || nw_path_is_constrained(path) };
        super::EXPENSIVE.store(costly, Ordering::Relaxed);
    }

    static DESCRIPTOR: Descriptor = Descriptor {
        reserved: 0,
        size: std::mem::size_of::<Block>(),
    };

    pub fn start() {
        // Kept for as long as the app runs, as the monitor is.
        let block: &'static Block = Box::leak(Box::new(Block {
            isa: &raw const _NSConcreteGlobalBlock,
            flags: BLOCK_IS_GLOBAL,
            reserved: 0,
            invoke: update,
            descriptor: &DESCRIPTOR,
        }));
        // SAFETY: a monitor made, given its handler and a queue, and
        // started, once; it is never released.
        unsafe {
            let m = nw_path_monitor_create();
            nw_path_monitor_set_update_handler(m, block);
            nw_path_monitor_set_queue(m, dispatch_get_global_queue(0, 0));
            nw_path_monitor_start(m);
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_network_is_watched_without_fault() {
        let _ = super::expensive();
        std::thread::sleep(std::time::Duration::from_millis(300));
        // A desk's network costs nothing; the call answers either way.
        let _ = super::expensive();
    }
}
