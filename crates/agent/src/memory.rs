//! The guest's memory handed back to the host: what it frees in pieces
//! (compact), and, while the environment asks for it (RECLAIM_FILE), what
//! its programs leave idle (offload) and DAMON's reclaim of untouched cache
//! over all of its RAM (damon).

use std::fs;
use std::io;
use std::path::Path;
use std::time::Duration;

/// A guest with swap hands back the memory its programs leave idle as
/// Meta's Senpai does (TMO): the agent asks the kernel again and again to
/// reclaim a little of the guest's memory, and the kernel takes what was
/// used least recently, page by page -- a program's memory into zswap's
/// compressed pool and on to the swap disk, its cache of files dropped. Free
/// page reporting hands what that frees to the host.
///
/// Asking costs the guest nothing while what is taken stays unused. When it
/// starts to cost, the guest's processes wait on memory to be brought back,
/// which the kernel's pressure stall information counts, and the agent asks
/// for less, then for nothing until the waiting stops.
///
/// Pages read back from a fast disk barely stall anything, so the pressure
/// alone lets the agent go on taking a guest's working set, which comes
/// straight back: an idle guest re-reading its files at megabytes a second,
/// its free memory scattered in pieces free page reporting cannot hand to
/// the host. The kernel counts what is read back (its refaults), some of it
/// long after it was taken. Once what was read back over the last minute or
/// so is a share of what was taken over it, what is left is in use, and the
/// agent waits before asking again, twice as long each time it finds the
/// same, and half as long each time it does not.
mod offload {
    use std::time::Duration;

    /// How often the agent asks.
    pub const EVERY: Duration = Duration::from_secs(5);
    /// How much of the guest's memory it asks for each time while nothing
    /// waits on memory: a program idle for a minute or two is out of
    /// memory, whatever its size.
    pub const STEP: f64 = 0.01;
    /// The share of the time some process may spend waiting on memory, at
    /// which the agent stops asking.
    pub const PRESSURE_TARGET: f64 = 0.001;
    /// The share of what was taken that may be read back before the agent
    /// stops asking.
    pub const REFAULT_SHARE: f64 = 0.25;
    /// How much of what was taken and read back carries over from one round
    /// to the next: about a minute's worth counts.
    pub const REFAULT_DECAY: f64 = 0.92;
    /// The longest the agent then waits before asking again.
    pub const MAX_HOLD: Duration = Duration::from_secs(300);
}

/// Says whether the environment hands back the memory its programs leave
/// idle and the cache nothing has touched: its spec's memory_reclaim, an
/// experimental setting, "1" while it is on. The worker writes it at each
/// boot and resume and whenever the setting changes, and the agent reads it
/// every round, so a running guest takes a change at once. A program left
/// idle has its memory taken, and is slow to answer when next used while it
/// is brought back in, so it is off unless asked for; what the guest frees
/// goes back to the host either way.
pub const RECLAIM_FILE: &str = "/run/hangar/memory-reclaim";

/// Whether reclaim is asked for.
fn reclaim_wanted() -> bool {
    reclaim_in(&fs::read_to_string(RECLAIM_FILE).unwrap_or_default())
}

/// Whether the reclaim file's content asks for reclaim.
pub fn reclaim_in(file: &str) -> bool {
    file.trim() == "1"
}

const MEMORY_RECLAIM: &str = "/sys/fs/cgroup/memory.reclaim";
const MEMORY_PRESSURE: &str = "/proc/pressure/memory";

/// Asks the kernel to reclaim, every [`offload::EVERY`], while the
/// environment asks for reclaim, for as long as the guest runs, turning
/// DAMON's reclaim on and off with it. A guest without swap is left to
/// DAMON, which reclaims only its cache.
pub async fn offload() {
    // A guest the kernel cannot be asked to reclaim in is still DAMON's.
    let can_ask = Path::new(MEMORY_RECLAIM).exists();
    let mut prev = memory_stall().unwrap_or(0);
    let page = page_size();
    let (mut taken, mut back) = reclaim_counts();
    // What was taken and read back over the last minute or so, in bytes.
    let (mut took_recently, mut back_recently) = (0.0f64, 0.0f64);
    // How many rounds the agent last waited, and how many it has still to
    // wait.
    let (mut hold, mut wait) = (0u32, 0u32);
    let max_hold = (offload::MAX_HOLD.as_secs() / offload::EVERY.as_secs()) as u32;
    let mut tick = tokio::time::interval(offload::EVERY);
    let mut on = false;
    loop {
        tick.tick().await;
        let want = reclaim_wanted();
        if want != on {
            on = want;
            damon(on);
            // Rounds from before the switch say nothing of this one.
            (took_recently, back_recently, hold, wait) = (0.0, 0.0, 0, 0);
            (taken, back) = reclaim_counts();
            if let Ok(s) = memory_stall() {
                prev = s;
            }
            continue;
        }
        if !on || !can_ask {
            continue;
        }
        let Ok(stall) = memory_stall() else {
            continue;
        };
        let pressure = stall.saturating_sub(prev) as f64 / offload::EVERY.as_micros() as f64;
        prev = stall;
        // What the last round took, and what this one read back.
        let (was_taken, was_back) = (taken, back);
        (taken, back) = reclaim_counts();
        let took = (taken - was_taken) * page;
        let read_back = (back - was_back) * page;
        took_recently = took_recently * offload::REFAULT_DECAY + took as f64;
        back_recently = back_recently * offload::REFAULT_DECAY + read_back as f64;
        if took > 0 && back_recently > took_recently * offload::REFAULT_SHARE {
            hold = (2 * hold).clamp(1, max_hold);
            wait = hold;
        } else if took > 0 {
            hold /= 2;
        }
        if wait > 0 {
            wait -= 1;
            continue;
        }
        if pressure >= offload::PRESSURE_TARGET {
            continue;
        }
        let (mem, swap) = memory_and_swap();
        if swap == 0 {
            continue;
        }
        let n = (mem as f64 * offload::STEP * (1.0 - pressure / offload::PRESSURE_TARGET)) as i64;
        if n < 1 << 20 {
            continue;
        }
        // EAGAIN is that it took less than asked: everything left is in use.
        if let Err(e) = fs::write(MEMORY_RECLAIM, n.to_string())
            && e.raw_os_error() != Some(libc::EAGAIN)
        {
            eprintln!("hangar-agent: offload: {e}");
        }
    }
}

/// The pages the agent's reclaim has taken since boot, and the pages read
/// back after reclaim took them (/proc/vmstat).
fn reclaim_counts() -> (i64, i64) {
    let vmstat = fs::read_to_string("/proc/vmstat").unwrap_or_default();
    let (mut taken, mut back) = (0, 0);
    for line in vmstat.lines() {
        let Some((key, v)) = line.split_once(' ') else {
            continue;
        };
        let n: i64 = v.trim().parse().unwrap_or(0);
        match key {
            "pgsteal_proactive" => taken += n,
            "workingset_refault_anon" | "workingset_refault_file" => back += n,
            _ => {}
        }
    }
    (taken, back)
}

/// How long, in microseconds since boot, some process has waited on
/// memory: the "some" line's total.
fn memory_stall() -> io::Result<u64> {
    let pressure = fs::read_to_string(MEMORY_PRESSURE)?;
    pressure
        .lines()
        .filter(|l| l.starts_with("some "))
        .flat_map(|l| l.split_whitespace())
        .find_map(|f| f.strip_prefix("total="))
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| io::Error::other(format!("no stall total in {MEMORY_PRESSURE}")))
}

/// The guest's memory and its swap, in bytes. Its memory changes as
/// virtio-mem plugs and unplugs it.
fn memory_and_swap() -> (i64, i64) {
    let info = fs::read_to_string("/proc/meminfo").unwrap_or_default();
    let (mut mem, mut swap) = (0, 0);
    for line in info.lines() {
        let Some((key, rest)) = line.split_once(':') else {
            continue;
        };
        let kib: i64 = rest
            .split_whitespace()
            .next()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        match key {
            "MemTotal" => mem = kib << 10,
            "SwapTotal" => swap = kib << 10,
            _ => {}
        }
    }
    (mem, swap)
}

/// Free page reporting hands the host only whole blocks of free memory (2
/// MiB with 4 KiB pages: the page reporting order), so memory the guest
/// frees a page here and a page there stays the host's to keep: hundreds of
/// MiB in an idle guest, what its reclaim and its programs left scattered.
/// Compacting moves the pages in use together and joins what is free into
/// blocks, which are then reported. The kernel compacts by itself only when
/// most of its free memory is scattered, and a guest has most of its free
/// memory in whole blocks however much is scattered, so the agent compacts
/// it.
///
/// Compacting a whole guest takes a second or two of a vCPU nested, far
/// less on real hardware, and some of what is scattered cannot be joined
/// (pages the kernel cannot move hold their blocks), so the agent compacts
/// only when what is scattered has grown by COMPACT_GROWTH since it last
/// did.
const COMPACT_EVERY: Duration = Duration::from_secs(60);
const COMPACT_GROWTH: i64 = 64 << 20;
const COMPACT_MEMORY: &str = "/proc/sys/vm/compact_memory";
const REPORTING_ORDER: &str = "/sys/module/page_reporting/parameters/page_reporting_order";

/// Compacts the guest's memory, every COMPACT_EVERY, when its scattered free
/// memory has grown by COMPACT_GROWTH.
pub async fn compact() {
    // No free page reporting: nothing the guest frees reaches the host.
    let Ok(order) = fs::read_to_string(REPORTING_ORDER) else {
        return;
    };
    let order: usize = match order.trim().parse() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("hangar-agent: compact: {REPORTING_ORDER}: {e}");
            return;
        }
    };
    let page = page_size();
    let scattered = || {
        scattered_free(
            &fs::read_to_string("/proc/buddyinfo").unwrap_or_default(),
            order,
            page,
        )
    };
    // What was scattered once the agent last compacted: what it cannot
    // join, and the mark it compacts again above.
    let mut after = 0;
    let mut tick = tokio::time::interval(COMPACT_EVERY);
    tick.tick().await;
    loop {
        tick.tick().await;
        if scattered() < after + COMPACT_GROWTH {
            continue;
        }
        if let Err(e) = fs::write(COMPACT_MEMORY, "1") {
            eprintln!("hangar-agent: compact: {e}");
            return;
        }
        after = scattered();
    }
}

/// The free memory in blocks smaller than 2^order pages, in bytes, from
/// /proc/buddyinfo's lines: "Node 0, zone   Normal  <free blocks of order
/// 0> <of order 1> ...".
pub fn scattered_free(buddyinfo: &str, order: usize, page: i64) -> i64 {
    let mut total = 0;
    for line in buddyinfo.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 4 {
            continue;
        }
        for (o, f) in fields[4..].iter().enumerate().take(order) {
            let n: i64 = f.parse().unwrap_or(0);
            total += (n << o) * page;
        }
    }
    total
}

/// Where DAMON's reclaim of pages the guest has not touched for a while is
/// set. The kernel's command line sets it up and leaves it off (ch.Config),
/// and the agent turns it on while the environment asks for reclaim.
const DAMON_RECLAIM: &str = "/sys/module/damon_reclaim/parameters";

/// Turns DAMON's reclaim on, watching all of the guest's RAM, or off. Left
/// to itself it watches the largest block the guest booted with, which
/// leaves out what virtio-mem plugs in: most of a resizable machine's
/// memory, and the whole of what it grows into.
fn damon(on: bool) {
    let dir = Path::new(DAMON_RECLAIM);
    if !dir.exists() {
        return;
    }
    if !on {
        if let Err(e) = fs::write(dir.join("enabled"), "N") {
            eprintln!("hangar-agent: reclaim: enabled: {e}");
        }
        return;
    }
    let span = fs::read_to_string("/proc/iomem")
        .map_err(|e| e.to_string())
        .and_then(|m| ram_span(&m));
    let (start, end) = match span {
        Ok(s) => s,
        Err(e) => {
            eprintln!("hangar-agent: reclaim: {e}");
            return;
        }
    };
    // The region is read as DAMON starts, so both ends are written first.
    for (name, value) in [
        ("monitor_region_start", start.to_string()),
        ("monitor_region_end", end.to_string()),
        ("enabled", "Y".to_string()),
    ] {
        if let Err(e) = fs::write(dir.join(name), value) {
            eprintln!("hangar-agent: reclaim: {name}: {e}");
            return;
        }
    }
}

/// The span of physical addresses the guest's RAM can be in, [start, end),
/// from its memory map (/proc/iomem): from the lowest RAM it booted with to
/// the end of what virtio-mem can plug in, which may be well beyond the RAM
/// it has. The span has holes, which are not RAM.
pub fn ram_span(iomem: &str) -> Result<(u64, u64), String> {
    let mut span: Option<(u64, u64)> = None;
    for line in iomem.lines() {
        // Nested resources are indented; only the top level is wanted.
        if line.is_empty() || line.starts_with(' ') {
            continue;
        }
        let Some((range, name)) = line.split_once(" : ") else {
            continue;
        };
        // virtio-mem's device region is the only virtio device at the top
        // of the map: the others are beneath the PCI bridge.
        let virtio_mem = name
            .strip_prefix("virtio")
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
        if name != "System RAM" && !virtio_mem {
            continue;
        }
        let Some((lo, hi)) = range.split_once('-') else {
            continue;
        };
        let (Ok(a), Ok(b)) = (u64::from_str_radix(lo, 16), u64::from_str_radix(hi, 16)) else {
            continue;
        };
        span = Some(match span {
            None => (a, b + 1),
            Some((s, e)) => (s.min(a), e.max(b + 1)),
        });
    }
    match span {
        // Unprivileged readers are shown every address as zero.
        None => Err("no RAM in the memory map".into()),
        Some((0, 1)) => {
            Err("the memory map's addresses are hidden: it must be read as root".into())
        }
        Some(s) => Ok(s),
    }
}

fn page_size() -> i64 {
    // SAFETY: sysconf has no preconditions.
    unsafe { libc::sysconf(libc::_SC_PAGESIZE) as i64 }
}
