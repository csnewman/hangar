//! Transparent huge pages for guest memory, on a Linux host.
//!
//! Cloud Hypervisor's guest memory is shared memory, and libkrun's is
//! anonymous memory it asks huge pages of. Memory a guest touches that the
//! host backs with 4 KiB pages runs slower at the second stage of
//! translation on any host, and under nested virtualisation is hundreds of
//! times slower to touch.

use crate::err;
use std::io;

/// The host's policy for transparent huge pages on shared memory.
const SHMEM_ENABLED: &str = "/sys/kernel/mm/transparent_hugepage/shmem_enabled";

/// Whether this host will give shared memory transparent huge pages.
///
/// It matters because nothing fails without them. Cloud Hypervisor asks --
/// it calls madvise(MADV_HUGEPAGE) on the guest's memory -- but that call
/// is ignored while the host's policy is "never", which is the value
/// distributions ship. The guest then runs on 4 KiB pages and boots in
/// three to five times as long, with no error anywhere to explain it. A
/// silent regression of that size is worth refusing to start for.
///
/// The file reads as a list with the active value in brackets, for example
///
/// ```text
/// always within_size advise [never] deny force
/// ```
///
/// "advise" is what Hangar wants: it grants huge pages only to callers that
/// ask, which is the monitor and nothing else on the host.
pub fn check() -> io::Result<()> {
    let b = match std::fs::read_to_string(SHMEM_ENABLED) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Err(err(format!(
                "this kernel has no {SHMEM_ENABLED}, so shared guest memory cannot take huge pages; \
                 Hangar needs a kernel built with CONFIG_TRANSPARENT_HUGEPAGE"
            )));
        }
        Err(e) => return Err(err(format!("reading {SHMEM_ENABLED}: {e}"))),
    };
    check_mode(&b)
}

/// check, given the policy file's text.
pub fn check_mode(text: &str) -> io::Result<()> {
    match selected(text) {
        "advise" | "always" | "within_size" | "force" => Ok(()),
        "" => Err(err(format!(
            "could not tell which mode is active in {SHMEM_ENABLED} ({:?})",
            text.trim()
        ))),
        // "never" and "deny" both leave the madvise with no effect.
        mode => Err(err(format!(
            "shared memory cannot take huge pages: {SHMEM_ENABLED} is {mode:?}.\n\
             Guest memory is always shared, so without this a guest boots three to five times slower \
             with nothing to show why.\n\
             Fix it for this boot with:\n    echo advise | sudo tee {SHMEM_ENABLED}\n\
             and permanently with a line in /etc/tmpfiles.d/hangar-thp.conf:\n    w {SHMEM_ENABLED} - - - - advise"
        ))),
    }
}

/// The value in brackets, which is the active one.
fn selected(s: &str) -> &str {
    s.split_whitespace()
        .find(|f| f.len() >= 2 && f.starts_with('[') && f.ends_with(']'))
        .map_or("", |f| &f[1..f.len() - 1])
}

/// The host's running totals of shared memory it has asked to back with a
/// huge page: those it could, and those it fell back to 4 KiB pages for
/// because no 2 MB block was free.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    pub allocated: u64,
    pub fell_back: u64,
}

/// The host's counts, from /proc/vmstat, where shared memory counts as file
/// memory. Fallbacks happen when the host's free memory is fragmented, and
/// the balloon makes them recur: memory a guest frees is handed back, and
/// taken again whenever the guest next uses it.
pub fn read_counts() -> io::Result<Counts> {
    let b = std::fs::read_to_string("/proc/vmstat")?;
    parse_counts(&b)
}

/// read_counts, given /proc/vmstat's text.
pub fn parse_counts(text: &str) -> io::Result<Counts> {
    let mut c = Counts::default();
    let mut seen = 0;
    for line in text.lines() {
        let Some((name, v)) = line.split_once(' ') else {
            continue;
        };
        let dst = match name {
            "thp_file_alloc" => &mut c.allocated,
            "thp_file_fallback" => &mut c.fell_back,
            _ => continue,
        };
        *dst = v
            .trim()
            .parse()
            .map_err(|e| err(format!("parsing {name} in /proc/vmstat: {e}")))?;
        seen += 1;
    }
    if seen != 2 {
        return Err(err(
            "/proc/vmstat has no thp_file_alloc and thp_file_fallback",
        ));
    }
    Ok(c)
}

/// How much of the host's free memory is in blocks at least as large as a
/// transparent huge page, which is all a guest's memory can take huge pages
/// from without the kernel first compacting.
pub fn free_huge_blocks_mib() -> io::Result<u64> {
    let pmd = std::fs::read_to_string("/sys/kernel/mm/transparent_hugepage/hpage_pmd_size")?;
    let huge: u64 = pmd.trim().parse().map_err(io::Error::other)?;
    let buddy = std::fs::read_to_string("/proc/buddyinfo")?;
    free_huge_blocks(&buddy, page_size(), huge).map(|b| b >> 20)
}

/// The bytes free in blocks of at least huge, from /proc/buddyinfo's text.
pub fn free_huge_blocks(buddyinfo: &str, page: u64, huge: u64) -> io::Result<u64> {
    let mut order = 0;
    let mut size = page;
    while size < huge {
        size *= 2;
        order += 1;
    }
    let mut bytes = 0u64;
    for line in buddyinfo.lines() {
        // Node 0, zone   Normal  <free blocks of order 0> <order 1> ...
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 4 {
            continue;
        }
        for (i, count) in f[4..].iter().enumerate() {
            if i < order {
                continue;
            }
            let n: u64 = count
                .parse()
                .map_err(|e| err(format!("parsing /proc/buddyinfo: {e}")))?;
            bytes += (n << i) * page;
        }
    }
    Ok(bytes)
}

/// Has the kernel move pages to make free memory contiguous again, so that
/// it can back a guest's memory with huge pages.
pub fn compact_memory() -> io::Result<()> {
    std::fs::write("/proc/sys/vm/compact_memory", "1")
}

fn page_size() -> u64 {
    // SAFETY: sysconf has no preconditions.
    unsafe { libc::sysconf(libc::_SC_PAGESIZE) as u64 }
}
