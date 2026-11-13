//! The machine a worker runs on: what it has, and how loaded it is.

use std::io;
use std::path::Path;
use std::sync::Mutex;

use hangar_api::{Resources, WorkerStats};

/// The machine's cumulative CPU time, in ticks: busy and in all.
#[derive(Debug, Clone, Copy, Default)]
struct CpuSample {
    busy: u64,
    total: u64,
}

/// Measures the machine a worker runs on. CPU use is the share of time busy
/// between one measurement and the next, so the first measurement has none
/// to report.
pub struct SysStats {
    /// On the filesystem whose space is reported: where environments are
    /// kept.
    path: std::path::PathBuf,
    last: Mutex<Option<CpuSample>>,
}

impl SysStats {
    pub fn new(path: impl Into<std::path::PathBuf>) -> SysStats {
        SysStats {
            path: path.into(),
            last: Mutex::new(None),
        }
    }

    pub fn measure(&self) -> Option<WorkerStats> {
        let (mut st, cpu) = read_system(&self.path)?;
        let mut last = self.last.lock().unwrap();
        if let Some(l) = *last
            && cpu.total > l.total
        {
            st.cpu_percent =
                100.0 * cpu.busy.saturating_sub(l.busy) as f64 / (cpu.total - l.total) as f64;
        }
        *last = Some(cpu);
        Some(st)
    }
}

/// The machine's CPUs and memory.
pub fn machine_capacity() -> io::Result<Resources> {
    let cpus = std::thread::available_parallelism().map_or(1, |n| n.get()) as i64;
    Ok(Resources {
        cpus,
        memory_mib: (memory_bytes()? >> 20) as i64,
    })
}

/// How much swap the machine has to count on, or 0 when that cannot be
/// read. macOS grows its swap as it likes, so none is counted on there to
/// overcommit memory.
pub fn swap_mib() -> i64 {
    #[cfg(target_os = "linux")]
    {
        meminfo("SwapTotal").map_or(0, |kib| kib / 1024)
    }
    #[cfg(not(target_os = "linux"))]
    {
        0
    }
}

/// The filesystem's space, at path or, before it exists, its nearest
/// ancestor, which is on the filesystem it will be on.
fn disk(path: &Path, used_from_avail: bool) -> Option<(i64, i64)> {
    let mut p = path;
    while !p.exists() {
        p = p.parent()?;
    }
    let c = std::ffi::CString::new(p.as_os_str().as_encoded_bytes()).ok()?;
    // SAFETY: statfs fills fs for a NUL-terminated path.
    let mut fs: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(c.as_ptr(), &mut fs) } != 0 {
        return None;
    }
    let bsize = fs.f_bsize as i64;
    let free = if used_from_avail {
        fs.f_bavail as i64
    } else {
        fs.f_bfree as i64
    };
    Some((
        fs.f_blocks as i64 * bsize,
        (fs.f_blocks as i64 - free) * bsize,
    ))
}

#[cfg(target_os = "linux")]
fn memory_bytes() -> io::Result<u64> {
    meminfo("MemTotal").map(|kib| kib as u64 * 1024)
}

/// One field of /proc/meminfo, in KiB.
#[cfg(target_os = "linux")]
fn meminfo(field: &str) -> io::Result<i64> {
    let text = std::fs::read_to_string("/proc/meminfo")?;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix(field).and_then(|r| r.strip_prefix(':')) {
            return rest
                .trim()
                .trim_end_matches("kB")
                .trim()
                .parse()
                .map_err(|e| io::Error::other(format!("parsing {field}: {e}")));
        }
    }
    Err(io::Error::other(format!("no {field} in /proc/meminfo")))
}

/// The machine's memory, load, disk and cumulative CPU time, from /proc and
/// statfs.
#[cfg(target_os = "linux")]
fn read_system(path: &Path) -> Option<(WorkerStats, CpuSample)> {
    let mut st = WorkerStats::default();
    let mut cpu = CpuSample::default();

    if let Ok(text) = std::fs::read_to_string("/proc/stat") {
        let f: Vec<&str> = text
            .lines()
            .next()
            .unwrap_or("")
            .split_whitespace()
            .collect();
        // cpu user nice system idle iowait irq softirq steal ...
        if f.len() >= 8 && f[0] == "cpu" {
            let v: Vec<u64> = f[1..8].iter().map(|s| s.parse().unwrap_or(0)).collect();
            cpu.total = v.iter().sum();
            cpu.busy = cpu.total - (v[3] + v[4]);
        }
    }
    if let Ok(text) = std::fs::read_to_string("/proc/loadavg") {
        st.load1 = text
            .split_whitespace()
            .next()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0.0);
    }
    if let Ok(text) = std::fs::read_to_string("/proc/meminfo") {
        let (mut total, mut avail, mut swap_total, mut swap_free) = (0i64, 0i64, 0i64, 0i64);
        for line in text.lines() {
            let Some((k, v)) = line.split_once(':') else {
                continue;
            };
            let kib: i64 = v.trim().trim_end_matches("kB").trim().parse().unwrap_or(0);
            match k {
                "MemTotal" => total = kib,
                "MemAvailable" => avail = kib,
                "SwapTotal" => swap_total = kib,
                "SwapFree" => swap_free = kib,
                _ => {}
            }
        }
        st.memory_total_mib = total / 1024;
        st.memory_used_mib = (total - avail) / 1024;
        st.swap_total_mib = swap_total / 1024;
        st.swap_used_mib = (swap_total - swap_free) / 1024;
    }
    if let Some((total, used)) = disk(path, false) {
        st.disk_total_bytes = total;
        st.disk_used_bytes = used;
    }
    Some((st, cpu))
}

#[cfg(target_os = "macos")]
fn sysctl<T: Copy>(name: &str) -> Option<T> {
    let c = std::ffi::CString::new(name).ok()?;
    let mut v = std::mem::MaybeUninit::<T>::zeroed();
    let mut len = std::mem::size_of::<T>();
    // SAFETY: sysctlbyname writes at most len bytes into v.
    let r = unsafe {
        libc::sysctlbyname(
            c.as_ptr(),
            v.as_mut_ptr().cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    // SAFETY: a successful call of the size asked for filled v.
    (r == 0 && len == std::mem::size_of::<T>()).then(|| unsafe { v.assume_init() })
}

#[cfg(target_os = "macos")]
fn memory_bytes() -> io::Result<u64> {
    sysctl::<u64>("hw.memsize").ok_or_else(|| io::Error::other("reading hw.memsize"))
}

/// The machine's cumulative CPU ticks, busy and in all.
#[cfg(target_os = "macos")]
#[allow(deprecated)]
fn cpu_times() -> CpuSample {
    let mut info: libc::host_cpu_load_info = unsafe { std::mem::zeroed() };
    let mut count = (std::mem::size_of::<libc::host_cpu_load_info>()
        / std::mem::size_of::<libc::integer_t>())
        as libc::mach_msg_type_number_t;
    // SAFETY: host_statistics fills info, count integers long.
    let r = unsafe {
        libc::host_statistics(
            libc::mach_host_self(),
            libc::HOST_CPU_LOAD_INFO,
            (&mut info as *mut libc::host_cpu_load_info).cast(),
            &mut count,
        )
    };
    if r != libc::KERN_SUCCESS {
        return CpuSample::default();
    }
    let t = |s: libc::c_int| info.cpu_ticks[s as usize] as u64;
    let idle = t(libc::CPU_STATE_IDLE);
    let total =
        idle + t(libc::CPU_STATE_USER) + t(libc::CPU_STATE_SYSTEM) + t(libc::CPU_STATE_NICE);
    CpuSample {
        busy: total - idle,
        total,
    }
}

/// struct loadavg.
#[cfg(target_os = "macos")]
#[repr(C)]
#[derive(Clone, Copy)]
struct LoadAvg {
    ldavg: [u32; 3],
    fscale: libc::c_long,
}

/// The machine's memory, load, disk and cumulative CPU time, from sysctl,
/// Mach and statfs.
#[cfg(target_os = "macos")]
fn read_system(path: &Path) -> Option<(WorkerStats, CpuSample)> {
    let mut st = WorkerStats::default();
    let cpu = cpu_times();

    // struct loadavg: three fixed-point averages, then their scale.
    if let Some(l) = sysctl::<LoadAvg>("vm.loadavg")
        && l.fscale > 0
    {
        st.load1 = l.ldavg[0] as f64 / l.fscale as f64;
    }
    if let Some(total) = sysctl::<u64>("hw.memsize") {
        st.memory_total_mib = (total >> 20) as i64;
        // The share of memory the kernel counts as available, as its
        // memory pressure gauge reports it.
        if let Some(level) = sysctl::<u32>("kern.memorystatus_level")
            && level <= 100
        {
            st.memory_used_mib = (total >> 20) as i64 * (100 - level as i64) / 100;
        }
    }
    if let Some(s) = sysctl::<libc::xsw_usage>("vm.swapusage") {
        st.swap_total_mib = (s.xsu_total >> 20) as i64;
        st.swap_used_mib = (s.xsu_used >> 20) as i64;
    }
    if let Some((total, used)) = disk(path, true) {
        st.disk_total_bytes = total;
        st.disk_used_bytes = used;
    }
    Some((st, cpu))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn memory_bytes() -> io::Result<u64> {
    Err(io::Error::other(
        "this machine's memory cannot be read here",
    ))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn read_system(_: &Path) -> Option<(WorkerStats, CpuSample)> {
    None
}
