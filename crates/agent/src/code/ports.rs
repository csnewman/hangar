//! The TCP ports the machine listens on, read from /proc/net/tcp and tcp6,
//! each with the program listening where the service may see its process:
//! the environment's user's own. Each is reachable through the
//! environment's numbered host, whose forwarder dials it on localhost.

use std::collections::{BTreeMap, HashMap};
use std::fs;

use serde::Serialize;

/// A port listened on.
#[derive(Debug, Serialize, PartialEq)]
pub struct Port {
    pub port: u16,
    /// Whether only the machine itself can reach it, not its network.
    pub loopback: bool,
    /// The program listening, when its process can be seen.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

/// A socket in a table: its address, port and inode.
struct Socket {
    loopback: bool,
    port: u16,
    inode: u64,
}

/// LISTEN, in the kernel's tcp_states.h.
const LISTEN: &str = "0A";

/// The listening sockets of one table, /proc/net/tcp's or tcp6's form: a
/// heading, then a line a socket, its local address hex IP:port.
fn listening(table: &str) -> Vec<Socket> {
    let mut out = Vec::new();
    for line in table.lines().skip(1) {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 10 || f[3] != LISTEN {
            continue;
        }
        let Some((addr, port)) = f[1].split_once(':') else {
            continue;
        };
        let (Ok(port), Ok(inode)) = (u16::from_str_radix(port, 16), f[9].parse::<u64>()) else {
            continue;
        };
        out.push(Socket {
            loopback: is_loopback(addr),
            port,
            inode,
        });
    }
    out
}

/// Whether a table's address is a loopback one: 127.0.0.0/8, ::1, or
/// ::ffff:127.0.0.0/104. The kernel prints the address's 32-bit words as
/// numbers in the host's byte order.
fn is_loopback(hex: &str) -> bool {
    let bytes: Vec<u8> = (0..hex.len() / 8)
        .filter_map(|i| u32::from_str_radix(&hex[i * 8..i * 8 + 8], 16).ok())
        .flat_map(u32::to_ne_bytes)
        .collect();
    match bytes.len() {
        4 => bytes[0] == 127,
        16 => {
            let v4_mapped = bytes[..10].iter().all(|&b| b == 0) && bytes[10..12] == [0xff, 0xff];
            (v4_mapped && bytes[12] == 127)
                || (bytes[..15].iter().all(|&b| b == 0) && bytes[15] == 1)
        }
        _ => false,
    }
}

/// Which process holds each socket inode, of those the service may read.
fn holders() -> HashMap<u64, u32> {
    let mut out = HashMap::new();
    let Ok(procs) = fs::read_dir("/proc") else {
        return out;
    };
    for p in procs.flatten() {
        let Some(pid) = p.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        let Ok(fds) = fs::read_dir(p.path().join("fd")) else {
            continue;
        };
        for fd in fds.flatten() {
            let Ok(link) = fs::read_link(fd.path()) else {
                continue;
            };
            if let Some(inode) = link
                .to_str()
                .and_then(|l| l.strip_prefix("socket:["))
                .and_then(|l| l.strip_suffix(']'))
                .and_then(|l| l.parse().ok())
            {
                out.entry(inode).or_insert(pid);
            }
        }
    }
    out
}

/// A process's command line, its arguments spaced.
fn command(pid: u32) -> Option<String> {
    let raw = fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    let line = raw
        .split(|&b| b == 0)
        .filter(|a| !a.is_empty())
        .map(|a| String::from_utf8_lossy(a).into_owned())
        .collect::<Vec<_>>()
        .join(" ");
    (!line.is_empty()).then_some(line)
}

/// The ports listened on, in order; a port listened on more than once
/// (IPv4 and IPv6) is one, reachable from the network if any of its
/// sockets is.
pub fn list() -> Vec<Port> {
    let mut sockets = Vec::new();
    for table in ["/proc/net/tcp", "/proc/net/tcp6"] {
        if let Ok(t) = fs::read_to_string(table) {
            sockets.extend(listening(&t));
        }
    }
    let holders = holders();
    let mut ports: BTreeMap<u16, Port> = BTreeMap::new();
    for s in sockets {
        let pid = holders.get(&s.inode).copied();
        let p = ports.entry(s.port).or_insert(Port {
            port: s.port,
            loopback: true,
            pid: None,
            command: None,
        });
        p.loopback &= s.loopback;
        if p.pid.is_none() && pid.is_some() {
            p.pid = pid;
            p.command = pid.and_then(command);
        }
    }
    ports.into_values().collect()
}
