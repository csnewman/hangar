//! What a running machine uses: read from inside the guest through its
//! agent, and from the host's processes.
//!
//! CPU time is the monitor process's on the host, which is what the
//! guest's vCPUs cost. The processes that serve it beside the monitor --
//! its GPU backend with everything that starts, its file system and its
//! network -- are measured apart, as support: the GPU's rendering happens
//! there, on the host's CPUs when it renders in software, and no vCPU limit
//! holds it. Memory, disk and network come from inside the guest: the guest
//! knows what it has free, and sees its own devices whichever backend
//! serves them. What its memory costs the host, and how big the machine is,
//! are the host's and the monitor's to say.

use std::collections::HashMap;
use std::time::Instant;

use hangar_api::EnvironmentStats;

/// The kernel's USER_HZ, the unit of /proc's CPU times. It is 100 on every
/// architecture Linux runs Hangar on.
pub const CLOCK_TICKS: f64 = 100.0;

/// The cumulative figures rates are worked out from.
#[derive(Debug, Clone, Copy)]
pub struct Counters {
    pub at: Instant,
    pub cpu_ticks: u64,
    pub gpu_ticks: u64,
    pub sup_ticks: u64,
    pub disk_read: u64,
    pub disk_write: u64,
    pub net_rx: u64,
    pub net_tx: u64,
    /// In pages.
    pub swap_in: u64,
    pub swap_out: u64,
}

impl Counters {
    pub fn new(at: Instant) -> Counters {
        Counters {
            at,
            cpu_ticks: 0,
            gpu_ticks: 0,
            sup_ticks: 0,
            disk_read: 0,
            disk_write: 0,
            net_rx: 0,
            net_tx: 0,
            swap_in: 0,
            swap_out: 0,
        }
    }
}

/// Sets st's rates from what changed between prev and next, on a machine of
/// cpus vCPUs whose pages are page bytes.
pub fn rates(prev: &Counters, next: &Counters, cpus: u32, page: f64, st: &mut EnvironmentStats) {
    let secs = next.at.duration_since(prev.at).as_secs_f64();
    let rate = |a: u64, b: u64| {
        if b < a || secs <= 0.0 {
            0.0
        } else {
            (b - a) as f64 / secs
        }
    };
    if cpus > 0 {
        st.cpu_percent = 100.0 * rate(prev.cpu_ticks, next.cpu_ticks) / CLOCK_TICKS / cpus as f64;
    }
    st.support_cpus = rate(prev.sup_ticks, next.sup_ticks) / CLOCK_TICKS;
    st.gpu_cpus = rate(prev.gpu_ticks, next.gpu_ticks) / CLOCK_TICKS;
    st.disk_read_bps = rate(prev.disk_read, next.disk_read);
    st.disk_write_bps = rate(prev.disk_write, next.disk_write);
    st.net_rx_bps = rate(prev.net_rx, next.net_rx);
    st.net_tx_bps = rate(prev.net_tx, next.net_tx);
    st.swap_in_bps = rate(prev.swap_in, next.swap_in) * page;
    st.swap_out_bps = rate(prev.swap_out, next.swap_out) * page;
}

/// Reads the guest's /proc/meminfo, /proc/net/dev, /proc/diskstats and
/// /proc/vmstat, concatenated.
pub fn guest(text: &str, st: &mut EnvironmentStats, c: &mut Counters) {
    let (mut total, mut avail, mut buffers, mut cached, mut shmem, mut locked) =
        (0i64, 0i64, 0i64, 0i64, 0i64, 0i64);
    let (mut swap_total, mut swap_free, mut zswap, mut zswapped) = (0i64, 0i64, 0i64, 0i64);
    // Pages swapped in and out, by zswap, its pool, and to and from the
    // disk; pswpout counts too what zswap writes back from its pool,
    // zswpwb, which had already left memory.
    let (mut pswpin, mut pswpout, mut zswpin, mut zswpout, mut zswpwb) =
        (0u64, 0u64, 0u64, 0u64, 0u64);
    let num = |s: &str| s.parse::<u64>().unwrap_or(0);
    for line in text.lines() {
        if let Some((key, rest)) = line.split_once(':') {
            let f: Vec<&str> = rest.split_whitespace().collect();
            if f.len() >= 16 {
                // net/dev: "  eth0: rxbytes rxpackets ... (8 fields) txbytes ..."
                if key.trim() != "lo" {
                    c.net_rx += num(f[0]);
                    c.net_tx += num(f[8]);
                }
                continue;
            }
            // meminfo: "MemTotal:   4028512 kB"
            if let Some(v) = f.first() {
                let kib: i64 = v.parse().unwrap_or(0);
                match key {
                    "MemTotal" => total = kib,
                    "MemAvailable" => avail = kib,
                    "Buffers" => buffers = kib,
                    "Cached" => cached = kib,
                    "Shmem" => shmem = kib,
                    "Mlocked" => locked = kib,
                    "SwapTotal" => swap_total = kib,
                    "SwapFree" => swap_free = kib,
                    "Zswap" => zswap = kib,
                    "Zswapped" => zswapped = kib,
                    _ => {}
                }
            }
            continue;
        }
        let f: Vec<&str> = line.split_whitespace().collect();
        // vmstat: "pswpin 1234"
        if f.len() == 2 {
            let n = num(f[1]);
            match f[0] {
                "pswpin" => pswpin = n,
                "pswpout" => pswpout = n,
                "zswpin" => zswpin = n,
                "zswpout" => zswpout = n,
                "zswpwb" => zswpwb = n,
                _ => {}
            }
            continue;
        }
        // diskstats: major minor name reads merged sectors ms writes merged sectors ...
        if f.len() >= 10 && is_whole_disk(f[2]) {
            c.disk_read += num(f[5]) * 512;
            c.disk_write += num(f[9]) * 512;
        }
    }
    st.memory_total_mib = total / 1024;
    st.memory_used_mib = (total - avail) / 1024;
    // Shared memory -- tmpfs, mostly -- is counted as cached, but it is
    // files nothing else holds, and it cannot be given up.
    st.memory_cached_mib = (buffers + cached - shmem).max(0) / 1024;
    st.memory_locked_mib = locked / 1024;
    st.swap_total_mib = swap_total / 1024;
    st.swap_used_mib = (swap_total - swap_free).max(0) / 1024;
    st.swap_compressed_mib = zswapped / 1024;
    st.swap_pool_mib = zswap / 1024;
    c.swap_in = pswpin + zswpin;
    c.swap_out = zswpout + pswpout - zswpwb.min(pswpout);
}

/// True for the guest's virtio disks themselves, and false for partitions,
/// which would count the same traffic twice.
fn is_whole_disk(name: &str) -> bool {
    name.len() == 3 && name.starts_with("vd")
}

/// A host process, as a sample reads it: its parent, the CPU time it has
/// used, and that of its children that have exited.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Proc {
    pub ppid: u32,
    pub ticks: u64,
    pub reaped: u64,
}

/// Every process on the host, read once for a sample.
#[derive(Debug, Clone, Default)]
pub struct HostProcs(pub HashMap<u32, Proc>);

impl HostProcs {
    /// The host's processes: Linux's /proc; none on a host without one.
    pub fn read() -> HostProcs {
        let mut out = HashMap::new();
        let Ok(rd) = std::fs::read_dir("/proc") else {
            return HostProcs(out);
        };
        for e in rd.flatten() {
            let Some(pid) = e.file_name().to_str().and_then(|n| n.parse::<u32>().ok()) else {
                continue;
            };
            let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
                continue;
            };
            if let Some(p) = parse_stat(&stat) {
                out.insert(pid, p);
            }
        }
        HostProcs(out)
    }

    /// A process and everything it started that still runs, the process
    /// first; none if it is not running.
    pub fn tree(&self, root: u32) -> Vec<u32> {
        if !self.0.contains_key(&root) {
            return vec![];
        }
        let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
        for (pid, p) in &self.0 {
            children.entry(p.ppid).or_default().push(*pid);
        }
        let mut out = vec![root];
        let mut stack = children.get(&root).cloned().unwrap_or_default();
        while let Some(pid) = stack.pop() {
            out.push(pid);
            if let Some(c) = children.get(&pid) {
                stack.extend(c);
            }
        }
        out
    }

    /// The CPU time a process and everything it started have used: those
    /// still running, and those it has waited for. A child that has not
    /// been waited for when it exits is lost from the count.
    pub fn tree_ticks(&self, root: u32) -> u64 {
        let tree = self.tree(root);
        if tree.is_empty() {
            return 0;
        }
        self.0[&root].reaped + tree.iter().map(|p| self.0[p].ticks).sum::<u64>()
    }

    /// What a process and everything it started hold beside a guest's RAM
    /// (process_memory's other). On macOS hangar-krun's memory is one
    /// footprint, guest RAM and all, and nothing is told apart.
    pub fn tree_memory(&self, root: u32) -> u64 {
        self.tree(root)
            .into_iter()
            .map(|p| process_memory(p).1)
            .sum()
    }
}

/// A /proc/<pid>/stat line's parent and CPU times. The command name is in
/// parentheses and may contain spaces, so fields are counted from after
/// its closing one: state(0) ppid(1) ... utime(11) stime(12) cutime(13)
/// cstime(14).
pub fn parse_stat(s: &str) -> Option<Proc> {
    let f: Vec<&str> = s[s.rfind(')')? + 1..].split_whitespace().collect();
    if f.len() < 15 {
        return None;
    }
    let n = |i: usize| f[i].parse::<u64>().unwrap_or(0);
    Some(Proc {
        ppid: f[1].parse().unwrap_or(0),
        ticks: n(11) + n(12),
        reaped: n(13) + n(14),
    })
}

/// The least a mapping of guest memory spans. A guest boots with at least
/// 1 GiB, in one mapping under libkrun (its RAM, the region virtio-mem
/// plugs into and the GPU's window beside them), and libkrun's own
/// mappings, its threads' arenas and the GPU's heaps, are smaller.
const GUEST_MAPPING: u64 = 1 << 30;

/// The memory a process holds, from its smaps' text: guest, what it maps
/// of a guest's RAM (Cloud Hypervisor's memory files, libkrun's mapping of
/// guest memory), and other, the rest of its anonymous and shared memory,
/// which is what serving the guest costs beside its RAM. Memory it shares
/// with another process (a GPU's buffers, mapped by its backend and the
/// monitor both) is counted in proportion (Pss), so that summing over the
/// processes counts it once. The files it maps, its program and libraries,
/// are the host's page cache and not counted.
pub fn parse_smaps(text: &str) -> (u64, u64) {
    #[derive(PartialEq)]
    enum Kind {
        File,
        Memory,
        Ram,
    }
    let (mut guest, mut other) = (0u64, 0u64);
    let mut kind = Kind::File;
    for line in text.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let Some(first) = fields.first() else {
            continue;
        };
        if let Some(span) = mapping_span(first) {
            // "start-end perms offset dev inode [name]"
            let name = if fields.len() >= 6 {
                fields[5..].join(" ")
            } else {
                String::new()
            };
            kind = if name.starts_with("/memfd:ch_ram")
                || (name.is_empty() && span >= GUEST_MAPPING)
            {
                Kind::Ram
            } else if name.is_empty()
                || ["[", "/memfd:", "/dev/shm/", "/SYSV", "/dev/dri/"]
                    .iter()
                    .any(|p| name.starts_with(p))
            {
                Kind::Memory
            } else {
                Kind::File
            };
            continue;
        }
        let into = match (&kind, *first) {
            (Kind::Ram, "Rss:") => &mut guest,
            (Kind::Memory, "Pss:") => &mut other,
            _ => continue,
        };
        if let Some(kib) = fields.get(1).and_then(|v| v.parse::<u64>().ok()) {
            *into += kib << 10;
        }
    }
    (guest, other)
}

/// The size of the mapping a smaps header's first field ("start-end", in
/// hex) describes, if it is one.
fn mapping_span(field: &str) -> Option<u64> {
    let (a, b) = field.split_once('-')?;
    let (start, end) = (
        u64::from_str_radix(a, 16).ok()?,
        u64::from_str_radix(b, 16).ok()?,
    );
    (end >= start).then(|| end - start)
}

/// A process's memory, as parse_smaps tells it apart; none on macOS.
pub fn process_memory(pid: u32) -> (u64, u64) {
    std::fs::read_to_string(format!("/proc/{pid}/smaps"))
        .map(|s| parse_smaps(&s))
        .unwrap_or((0, 0))
}

/// The host memory a monitor's guest RAM takes up: on Linux, the pages held
/// by the memory files the monitor maps it from, each file counted once
/// however many times it is open (a page the guest hands back is punched
/// out of its file, so this falls as it does), or for libkrun, which maps
/// it from no file, the anonymous mapping holding it; on macOS,
/// hangar-krun's footprint, whose guest memory is its own.
pub fn guest_ram(pid: u32) -> u64 {
    #[cfg(target_os = "macos")]
    {
        usage(pid).1
    }
    #[cfg(not(target_os = "macos"))]
    {
        let n = memfd_ram(pid);
        if n > 0 { n } else { process_memory(pid).0 }
    }
}

#[cfg(not(target_os = "macos"))]
fn memfd_ram(pid: u32) -> u64 {
    use std::os::unix::fs::MetadataExt;
    let dir = format!("/proc/{pid}/fd");
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return 0;
    };
    let mut seen = std::collections::HashSet::new();
    let mut total = 0;
    for e in rd.flatten() {
        let fd = e.path();
        if !std::fs::read_link(&fd).is_ok_and(|t| t.to_string_lossy().starts_with("/memfd:ch_ram"))
        {
            continue;
        }
        let Ok(m) = std::fs::metadata(&fd) else {
            continue;
        };
        if seen.insert(m.ino()) {
            total += m.blocks() * 512;
        }
    }
    total
}

/// A process's user and system CPU time, in clock ticks.
pub fn process_ticks(pid: u32) -> u64 {
    #[cfg(target_os = "macos")]
    {
        usage(pid).0 / (1_000_000_000 / CLOCK_TICKS as u64)
    }
    #[cfg(not(target_os = "macos"))]
    {
        std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .ok()
            .and_then(|s| parse_stat(&s))
            .map_or(0, |p| p.ticks)
    }
}

/// A process's CPU time, in nanoseconds, and its physical footprint: for
/// hangar-krun, nearly all of it the guest's memory.
#[cfg(target_os = "macos")]
fn usage(pid: u32) -> (u64, u64) {
    // SAFETY: ri is written by proc_pid_rusage, tb by mach_timebase_info.
    unsafe {
        let mut ri: libc::rusage_info_v2 = std::mem::zeroed();
        if libc::proc_pid_rusage(
            pid as libc::c_int,
            libc::RUSAGE_INFO_V2,
            (&mut ri as *mut libc::rusage_info_v2).cast(),
        ) != 0
        {
            return (0, 0);
        }
        let mut tb = Timebase { numer: 1, denom: 1 };
        mach_timebase_info(&mut tb);
        let cpu = (ri.ri_user_time + ri.ri_system_time) * tb.numer as u64 / tb.denom.max(1) as u64;
        (cpu, ri.ri_phys_footprint)
    }
}

/// mach_timebase_info's, from mach/mach_time.h: how many nanoseconds a
/// tick of mach_absolute_time is, as a fraction.
#[cfg(target_os = "macos")]
#[repr(C)]
struct Timebase {
    numer: u32,
    denom: u32,
}

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn mach_timebase_info(info: *mut Timebase) -> libc::c_int;
}

/// The space a file takes on disk: its allocated blocks, which for a sparse
/// image is what it holds rather than how big it claims to be.
pub fn allocated(path: &std::path::Path) -> u64 {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path).map_or(0, |m| m.blocks() * 512)
}
