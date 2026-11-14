//! Times as the control plane's Go code reads them.

use std::time::{SystemTime, UNIX_EPOCH};

/// A time in RFC 3339, in UTC, to the nanosecond with trailing zeros left
/// off, as Go writes one.
pub fn rfc3339(t: SystemTime) -> String {
    let d = t.duration_since(UNIX_EPOCH).unwrap_or_default();
    // time_t is 64-bit on every target the agent is built for.
    let secs = d.as_secs() as i64;
    // SAFETY: tm is plain data, which gmtime_r fills in.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::gmtime_r(&secs, &mut tm) };
    let mut s = format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    );
    let nanos = d.subsec_nanos();
    if nanos > 0 {
        let frac = format!("{nanos:09}");
        s.push('.');
        s.push_str(frac.trim_end_matches('0'));
    }
    s.push('Z');
    s
}
