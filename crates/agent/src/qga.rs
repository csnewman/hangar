//! QEMU's guest agent protocol, as much of it as Proxmox asks of a VM
//! whose agent channel is on (`agent: 1`, the virtio-serial port
//! org.qemu.guest_agent.0). Proxmox's API uses it to shut the VM down
//! cleanly, to show its addresses and its OS, and to write a file into it:
//! the worker writes the hub's settings for this boot, secret and all
//! (`crate::hub::SETTINGS`), and a worker VM's maker writes its settings
//! (`crate::worker::SETTINGS`); no other file may be opened.
//!
//! Requests are JSON objects, `{"execute": name, "arguments": {...}}`, one
//! after another on the port, each answered with `{"return": ...}` or
//! `{"error": {"class", "desc"}}` and a newline. guest-sync-delimited's
//! answer starts with a 0xFF byte, which shows the host where the answers
//! to its own requests begin; a 0xFF from the host drops whatever was read
//! before it.

use std::collections::HashMap;
use std::io::{self, Write};
use std::os::fd::{FromRawFd, OwnedFd};
use std::time::Duration;

use base64::Engine;
use serde_json::{Value, json};

use crate::fdio::FdStream;

/// The agent channel's port, by the name QEMU gives it.
const PORT_NAME: &str = "org.qemu.guest_agent.0";

/// How long the port is looked for: a VM without one has none to find.
const PORT_WITHIN: Duration = Duration::from_secs(30);

/// The files the host may write, and no others.
const WRITABLE: [&str; 2] = [crate::hub::SETTINGS, crate::worker::SETTINGS];

/// What a request may name, each answered unless it is guest-shutdown.
const COMMANDS: [&str; 15] = [
    "guest-sync-delimited",
    "guest-sync",
    "guest-ping",
    "guest-info",
    "guest-get-osinfo",
    "guest-get-host-name",
    "guest-get-time",
    "guest-network-get-interfaces",
    "guest-fsfreeze-status",
    "guest-file-open",
    "guest-file-write",
    "guest-file-flush",
    "guest-file-close",
    "guest-shutdown",
    "guest-get-timezone",
];

/// Answers the agent channel for as long as the guest runs, if the VM has
/// one.
pub async fn serve() {
    let Some(port) = find().await else { return };
    let mut s = Session::default();
    let mut buf = Vec::new();
    let mut chunk = vec![0u8; 16 << 10];
    loop {
        // With no host on the other end, the port reads as ended until one
        // connects: Proxmox connects for each request it makes.
        let n = match port.read(&mut chunk).await {
            Ok(0) | Err(_) => {
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
            Ok(n) => n,
        };
        buf.extend_from_slice(&chunk[..n]);
        for answer in s.take(&mut buf) {
            if port.write_all(&answer).await.is_err() {
                break;
            }
        }
    }
}

/// The port, found by its name in sysfs, as udev's link to it may not be
/// made yet while the agent starts.
async fn find() -> Option<FdStream> {
    let deadline = tokio::time::Instant::now() + PORT_WITHIN;
    loop {
        if let Ok(dir) = std::fs::read_dir("/sys/class/virtio-ports") {
            for e in dir.flatten() {
                let name = std::fs::read_to_string(e.path().join("name")).unwrap_or_default();
                if name.trim() == PORT_NAME {
                    let dev = format!("/dev/{}", e.file_name().to_string_lossy());
                    match open(&dev) {
                        Ok(p) => return Some(p),
                        Err(e) => eprintln!("hangar-agent: guest agent channel {dev}: {e}"),
                    }
                }
            }
        }
        if tokio::time::Instant::now() > deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

fn open(dev: &str) -> io::Result<FdStream> {
    let path = std::ffi::CString::new(dev).map_err(io::Error::other)?;
    // SAFETY: open(2) of a NUL-terminated path; the descriptor is owned at
    // once.
    let fd = unsafe { libc::open(path.as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a descriptor open(2) just returned, owned by no one else.
    FdStream::new(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// One run of the protocol: the files open, by handle.
#[derive(Default)]
pub struct Session {
    files: HashMap<i64, std::fs::File>,
    next: i64,
}

impl Session {
    /// Takes the requests complete in buf, leaving what is not yet whole,
    /// and gives each answer as it goes down the port.
    pub fn take(&mut self, buf: &mut Vec<u8>) -> Vec<Vec<u8>> {
        let mut answers = vec![];
        if let Some(at) = buf.iter().rposition(|&b| b == 0xff) {
            buf.drain(..=at);
        }
        loop {
            let start = buf
                .iter()
                .position(|b| !b.is_ascii_whitespace())
                .unwrap_or(buf.len());
            buf.drain(..start);
            if buf.is_empty() {
                break;
            }
            let mut it = serde_json::Deserializer::from_slice(buf).into_iter::<Value>();
            match it.next() {
                Some(Ok(v)) => {
                    let used = it.byte_offset();
                    buf.drain(..used);
                    if let Some(a) = self.answer(&v) {
                        answers.push(a);
                    }
                }
                Some(Err(e)) if e.is_eof() => break,
                _ => {
                    buf.clear();
                    break;
                }
            }
        }
        answers
    }

    /// The answer to one request, as it goes down the port; none for
    /// guest-shutdown, which answers by going.
    fn answer(&mut self, req: &Value) -> Option<Vec<u8>> {
        let name = req["execute"].as_str().unwrap_or_default();
        let args = &req["arguments"];
        let result = match name {
            "guest-sync-delimited" => {
                let mut out = vec![0xff];
                out.extend(line(&json!({"return": args["id"]})));
                return Some(out);
            }
            "guest-sync" => Ok(args["id"].clone()),
            "guest-ping" => Ok(json!({})),
            "guest-info" => Ok(info()),
            "guest-get-osinfo" => Ok(os_info()),
            "guest-get-host-name" => Ok(json!({"host-name": host_name()})),
            "guest-get-time" => Ok(json!(now_nanos())),
            "guest-get-timezone" => Ok(json!({"offset": 0, "zone": "UTC"})),
            "guest-network-get-interfaces" => Ok(interfaces()),
            "guest-fsfreeze-status" => Ok(json!("thawed")),
            "guest-file-open" => self.file_open(args),
            "guest-file-write" => self.file_write(args),
            "guest-file-flush" => Ok(json!({})),
            "guest-file-close" => {
                let h = args["handle"].as_i64().unwrap_or(-1);
                match self.files.remove(&h) {
                    Some(f) => f.sync_all().map(|_| json!({})).map_err(generic),
                    None => Err(generic("no such file handle")),
                }
            }
            "guest-shutdown" => {
                let verb = match args["mode"].as_str().unwrap_or("powerdown") {
                    "reboot" => "reboot",
                    "halt" => "halt",
                    _ => "poweroff",
                };
                // A worker VM's agent is its whole system, PID 1.
                #[cfg(target_os = "linux")]
                if std::process::id() == 1 {
                    crate::worker::power_off(verb);
                    return None;
                }
                let _ = std::process::Command::new("systemctl").arg(verb).spawn();
                return None;
            }
            other => Err(json!({
                "class": "CommandNotFound",
                "desc": format!("The command {other} has not been found"),
            })),
        };
        Some(match result {
            Ok(r) => line(&json!({"return": r})),
            Err(e) => line(&json!({"error": e})),
        })
    }

    fn file_open(&mut self, args: &Value) -> Result<Value, Value> {
        let path = args["path"].as_str().unwrap_or_default();
        let mode = args["mode"].as_str().unwrap_or("r");
        if !WRITABLE.contains(&path) || !mode.starts_with('w') {
            return Err(generic(
                "Hangar's agent opens no file but its settings, to write",
            ));
        }
        let f = write_private(path).map_err(generic)?;
        // Opened empty: the agent tries its link to the hub, which is dead
        // if the guest has just resumed, and reads the settings again as it
        // dials.
        #[cfg(target_os = "linux")]
        if path == crate::hub::SETTINGS {
            crate::hub::settings_written();
        }
        self.next += 1;
        self.files.insert(self.next, f);
        Ok(json!(self.next))
    }

    fn file_write(&mut self, args: &Value) -> Result<Value, Value> {
        // Proxmox's base64 is Perl's, broken into lines, as QEMU's own
        // agent takes it.
        let text: String = args["buf-b64"]
            .as_str()
            .unwrap_or_default()
            .chars()
            .filter(|c| !c.is_ascii_whitespace())
            .collect();
        let b = base64::engine::general_purpose::STANDARD
            .decode(text)
            .map_err(generic)?;
        let h = args["handle"].as_i64().unwrap_or(-1);
        let f = self
            .files
            .get_mut(&h)
            .ok_or_else(|| generic("no such file handle"))?;
        let n = args["count"]
            .as_u64()
            .map_or(b.len(), |c| (c as usize).min(b.len()));
        f.write_all(&b[..n]).map_err(generic)?;
        Ok(json!({"count": n, "eof": false}))
    }
}

fn line(v: &Value) -> Vec<u8> {
    let mut b = serde_json::to_vec(v).unwrap_or_default();
    b.push(b'\n');
    b
}

fn generic(e: impl std::fmt::Display) -> Value {
    json!({"class": "GenericError", "desc": e.to_string()})
}

/// Opens path to write, made empty, readable by root alone, in a directory
/// that is root's alone.
fn write_private(path: &str) -> io::Result<std::fs::File> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    if let Some(dir) = std::path::Path::new(path).parent() {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
    }
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
}

fn info() -> Value {
    let commands: Vec<Value> = COMMANDS
        .iter()
        .map(|c| json!({"name": c, "enabled": true, "success-response": *c != "guest-shutdown"}))
        .collect();
    json!({"version": crate::service::BUILD, "supported_commands": commands})
}

fn host_name() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .unwrap_or_default()
        .trim()
        .to_string()
}

fn now_nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default()
}

/// The guest's OS, from /etc/os-release and the kernel, as QEMU's agent
/// gives it.
fn os_info() -> Value {
    let release = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
    let field = |k: &str| {
        release
            .lines()
            .find_map(|l| l.strip_prefix(&format!("{k}=")))
            .map(|v| v.trim_matches('"').to_string())
            .unwrap_or_default()
    };
    let read = |p: &str| {
        std::fs::read_to_string(p)
            .unwrap_or_default()
            .trim()
            .to_string()
    };
    json!({
        "id": field("ID"),
        "name": field("NAME"),
        "pretty-name": field("PRETTY_NAME"),
        "version": field("VERSION"),
        "version-id": field("VERSION_ID"),
        "kernel-release": read("/proc/sys/kernel/osrelease"),
        "kernel-version": read("/proc/sys/kernel/version"),
        "machine": std::env::consts::ARCH,
    })
}

/// The guest's network interfaces and their addresses, as QEMU's agent
/// gives them.
fn interfaces() -> Value {
    let mut by_name: Vec<(String, Vec<Value>)> = vec![];
    let mut add = |name: &str, addr: Option<Value>| {
        let at = match by_name.iter().position(|(n, _)| n == name) {
            Some(i) => i,
            None => {
                by_name.push((name.to_string(), vec![]));
                by_name.len() - 1
            }
        };
        if let Some(a) = addr {
            by_name[at].1.push(a);
        }
    };
    let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs fills head with a list freed below.
    if unsafe { libc::getifaddrs(&mut head) } == 0 {
        let mut cur = head;
        while !cur.is_null() {
            // SAFETY: cur is an entry of the list getifaddrs gave.
            let ifa = unsafe { &*cur };
            cur = ifa.ifa_next;
            // SAFETY: ifa_name is a NUL-terminated string.
            let name = unsafe { std::ffi::CStr::from_ptr(ifa.ifa_name) }
                .to_string_lossy()
                .into_owned();
            add(&name, address(ifa.ifa_addr, ifa.ifa_netmask));
        }
        // SAFETY: head is the list getifaddrs gave.
        unsafe { libc::freeifaddrs(head) };
    }
    Value::Array(
        by_name
            .into_iter()
            .map(|(name, addrs)| {
                let mac = std::fs::read_to_string(format!("/sys/class/net/{name}/address"))
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                json!({"name": name, "hardware-address": mac, "ip-addresses": addrs})
            })
            .collect(),
    )
}

/// An interface's address and prefix, if it is IPv4 or IPv6.
fn address(addr: *const libc::sockaddr, mask: *const libc::sockaddr) -> Option<Value> {
    if addr.is_null() {
        return None;
    }
    // SAFETY: addr points to a sockaddr of the family it names, as does
    // mask when it is not null.
    unsafe {
        match (*addr).sa_family as i32 {
            libc::AF_INET => {
                let a = &*(addr as *const libc::sockaddr_in);
                let ip = std::net::Ipv4Addr::from(u32::from_be(a.sin_addr.s_addr));
                let prefix = if mask.is_null() {
                    32
                } else {
                    u32::from_be((*(mask as *const libc::sockaddr_in)).sin_addr.s_addr).count_ones()
                };
                Some(
                    json!({"ip-address-type": "ipv4", "ip-address": ip.to_string(), "prefix": prefix}),
                )
            }
            libc::AF_INET6 => {
                let a = &*(addr as *const libc::sockaddr_in6);
                let ip = std::net::Ipv6Addr::from(a.sin6_addr.s6_addr);
                let prefix: u32 = if mask.is_null() {
                    128
                } else {
                    (*(mask as *const libc::sockaddr_in6))
                        .sin6_addr
                        .s6_addr
                        .iter()
                        .map(|b| b.count_ones())
                        .sum()
                };
                Some(
                    json!({"ip-address-type": "ipv6", "ip-address": ip.to_string(), "prefix": prefix}),
                )
            }
            _ => None,
        }
    }
}
