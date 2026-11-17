//! A DHCPv4 client, for a worker VM, where the agent is the whole system
//! (`crate::worker`): it finds the network card, asks the network's DHCP
//! server for an address (RFC 2131), puts it, the netmask and the default
//! route on the card, writes /etc/resolv.conf, and keeps the lease, renewing
//! it with the server that gave it at half its time, then with any server,
//! and starting over if it runs out or is refused.
//!
//! It speaks over a UDP socket bound to port 68 on the card alone, which can
//! send broadcasts before the card has an address, and asks servers to
//! broadcast their answers (the BROADCAST flag), which the socket then
//! receives. The client identifier is the card's MAC, so the server gives
//! the same address back.

use std::io;
use std::net::{Ipv4Addr, SocketAddrV4};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::time::Duration;

use tokio::net::UdpSocket;
use tokio::sync::watch;
use tokio::time::Instant;

const SERVER_PORT: u16 = 67;
const CLIENT_PORT: u16 = 68;
const COOKIE: [u8; 4] = [99, 130, 83, 99];

const DISCOVER: u8 = 1;
const OFFER: u8 = 2;
const REQUEST: u8 = 3;
const ACK: u8 = 5;
const NAK: u8 = 6;

const OPT_MASK: u8 = 1;
const OPT_ROUTER: u8 = 3;
const OPT_DNS: u8 = 6;
const OPT_HOSTNAME: u8 = 12;
const OPT_DOMAIN: u8 = 15;
const OPT_REQUESTED: u8 = 50;
const OPT_LEASE: u8 = 51;
const OPT_TYPE: u8 = 53;
const OPT_SERVER: u8 = 54;
const OPT_PARAMS: u8 = 55;
const OPT_RENEW: u8 = 58;
const OPT_REBIND: u8 = 59;
const OPT_CLIENT_ID: u8 = 61;

/// How long one request waits for its answer.
const ANSWER_WITHIN: Duration = Duration::from_secs(4);
/// The longest wait between tries to get an address.
const RETRY_AT_MOST: Duration = Duration::from_secs(64);

/// An address the network's DHCP server has given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lease {
    pub address: Ipv4Addr,
    pub prefix: u8,
    pub router: Option<Ipv4Addr>,
    pub dns: Vec<Ipv4Addr>,
    pub domain: Option<String>,
    server: Ipv4Addr,
    time: Duration,
    renew: Duration,
    rebind: Duration,
}

/// Gets and keeps an address for the machine's network card, for as long as
/// the system runs, saying on lease each one it holds (none while it has
/// none). hostname is what the server is told the machine is called.
pub async fn run(hostname: String, lease: watch::Sender<Option<Lease>>) {
    let iface = card().await;
    let mac = match mac_of(&iface) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("hangar-agent: dhcp: {iface}: {e}");
            return std::future::pending().await;
        }
    };
    for name in ["lo", iface.as_str()] {
        if let Err(e) = up(name) {
            eprintln!("hangar-agent: dhcp: bringing {name} up: {e}");
        }
    }
    let sock = loop {
        match socket(&iface) {
            Ok(s) => break s,
            Err(e) => {
                eprintln!("hangar-agent: dhcp: {iface}: {e}");
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    };
    let c = Client {
        sock,
        mac,
        hostname,
    };
    loop {
        let (mut got, mut at) = c.obtain().await;
        if let Err(e) = configure(&iface, &got, None) {
            eprintln!(
                "hangar-agent: dhcp: putting {} on {iface}: {e}",
                got.address
            );
            tokio::time::sleep(Duration::from_secs(5)).await;
            continue;
        }
        eprintln!(
            "hangar-agent: dhcp: {iface} has {}/{} for {}s",
            got.address,
            got.prefix,
            got.time.as_secs()
        );
        lease.send_replace(Some(got.clone()));
        // Bound: renewed at T1 with its server, at T2 with any, until it
        // runs out.
        loop {
            tokio::time::sleep_until(at + got.renew).await;
            match c.keep(&got, at).await {
                Kept::Renewed(new) => {
                    if new.address != got.address
                        && let Err(e) = configure(&iface, &new, Some(&got))
                    {
                        eprintln!("hangar-agent: dhcp: moving to {}: {e}", new.address);
                    }
                    got = new;
                    at = Instant::now();
                    lease.send_replace(Some(got.clone()));
                }
                Kept::Lost(why) => {
                    eprintln!("hangar-agent: dhcp: lost {}: {why}", got.address);
                    let _ = deconfigure(&iface);
                    lease.send_replace(None);
                    break;
                }
            }
        }
    }
}

enum Kept {
    Renewed(Lease),
    Lost(&'static str),
}

struct Client {
    sock: UdpSocket,
    mac: [u8; 6],
    hostname: String,
}

impl Client {
    /// An address, asked for until one is given, and when.
    async fn obtain(&self) -> (Lease, Instant) {
        let mut wait = ANSWER_WITHIN;
        loop {
            match self.try_obtain().await {
                Ok(Some(l)) => return (l, Instant::now()),
                Ok(None) => {}
                Err(e) => eprintln!("hangar-agent: dhcp: {e}"),
            }
            tokio::time::sleep(wait).await;
            wait = (wait * 2).min(RETRY_AT_MOST);
        }
    }

    /// One discover, offer, request and acknowledgement.
    async fn try_obtain(&self) -> io::Result<Option<Lease>> {
        let xid = xid();
        let everyone = SocketAddrV4::new(Ipv4Addr::BROADCAST, SERVER_PORT);
        let discover = self.message(DISCOVER, xid, Ipv4Addr::UNSPECIFIED, &[]);
        let Some(offer) = self.exchange(everyone, &discover, xid, &[OFFER]).await? else {
            return Ok(None);
        };
        let Some(server) = offer.ip(OPT_SERVER) else {
            return Ok(None);
        };
        let request = self.message(
            REQUEST,
            xid,
            Ipv4Addr::UNSPECIFIED,
            &[
                (OPT_REQUESTED, offer.yiaddr.octets().to_vec()),
                (OPT_SERVER, server.octets().to_vec()),
            ],
        );
        match self.exchange(everyone, &request, xid, &[ACK, NAK]).await? {
            Some(a) if a.kind == ACK => Ok(a.lease()),
            _ => Ok(None),
        }
    }

    /// Renews a lease got at at: with its server until T2, then with any
    /// until it runs out.
    async fn keep(&self, l: &Lease, at: Instant) -> Kept {
        let mut to = SocketAddrV4::new(l.server, SERVER_PORT);
        loop {
            let now = Instant::now();
            if now >= at + l.time {
                return Kept::Lost("it ran out");
            }
            if now >= at + l.rebind {
                to = SocketAddrV4::new(Ipv4Addr::BROADCAST, SERVER_PORT);
            }
            let xid = xid();
            let request = self.message(REQUEST, xid, l.address, &[]);
            match self.exchange(to, &request, xid, &[ACK, NAK]).await {
                Ok(Some(a)) if a.kind == NAK => return Kept::Lost("the server refused it"),
                Ok(Some(a)) => {
                    if let Some(new) = a.lease() {
                        return Kept::Renewed(new);
                    }
                }
                Ok(None) => {}
                Err(e) => eprintln!("hangar-agent: dhcp: renewing: {e}"),
            }
            // Half the time left to the next stage, at least a minute
            // (RFC 2131 4.4.5), or what is left of the lease.
            let next = if Instant::now() < at + l.rebind {
                at + l.rebind
            } else {
                at + l.time
            };
            let left = next.saturating_duration_since(Instant::now());
            let wait = (left / 2).max(Duration::from_secs(60)).min(left);
            tokio::time::sleep(wait.max(Duration::from_secs(1))).await;
        }
    }

    /// Sends msg to to and waits for an answer to it of a kind in want.
    async fn exchange(
        &self,
        to: SocketAddrV4,
        msg: &[u8],
        xid: u32,
        want: &[u8],
    ) -> io::Result<Option<Reply>> {
        self.sock.send_to(msg, to).await?;
        let deadline = Instant::now() + ANSWER_WITHIN;
        let mut buf = vec![0u8; 1500];
        loop {
            let got = tokio::time::timeout_at(deadline, self.sock.recv_from(&mut buf)).await;
            let Ok(got) = got else { return Ok(None) };
            let (n, _) = got?;
            if let Some(r) = parse(&buf[..n], xid, &self.mac)
                && want.contains(&r.kind)
            {
                return Ok(Some(r));
            }
        }
    }

    /// A client's message: BOOTP's fixed part, the cookie, and options.
    fn message(&self, kind: u8, xid: u32, ciaddr: Ipv4Addr, extra: &[(u8, Vec<u8>)]) -> Vec<u8> {
        let mut b = vec![0u8; 236];
        b[0] = 1; // BOOTREQUEST
        b[1] = 1; // Ethernet
        b[2] = 6;
        b[4..8].copy_from_slice(&xid.to_be_bytes());
        // Answers broadcast while the card has no address to receive them on.
        if ciaddr.is_unspecified() {
            b[10] = 0x80;
        }
        b[12..16].copy_from_slice(&ciaddr.octets());
        b[28..34].copy_from_slice(&self.mac);
        b.extend_from_slice(&COOKIE);
        let mut opt = |code: u8, data: &[u8]| {
            b.push(code);
            b.push(data.len() as u8);
            b.extend_from_slice(data);
        };
        opt(OPT_TYPE, &[kind]);
        let mut id = vec![1];
        id.extend_from_slice(&self.mac);
        opt(OPT_CLIENT_ID, &id);
        if !self.hostname.is_empty() {
            opt(OPT_HOSTNAME, self.hostname.as_bytes());
        }
        opt(
            OPT_PARAMS,
            &[
                OPT_MASK, OPT_ROUTER, OPT_DNS, OPT_DOMAIN, OPT_LEASE, OPT_RENEW, OPT_REBIND,
            ],
        );
        for (code, data) in extra {
            opt(*code, data);
        }
        b.push(255);
        // Some servers want a message at least as long as BOOTP's.
        if b.len() < 300 {
            b.resize(300, 0);
        }
        b
    }
}

/// A server's answer.
struct Reply {
    kind: u8,
    yiaddr: Ipv4Addr,
    options: Vec<(u8, Vec<u8>)>,
}

impl Reply {
    fn opt(&self, code: u8) -> Option<&[u8]> {
        self.options
            .iter()
            .find(|(c, _)| *c == code)
            .map(|(_, d)| d.as_slice())
    }

    fn ip(&self, code: u8) -> Option<Ipv4Addr> {
        let d = self.opt(code)?;
        (d.len() >= 4).then(|| Ipv4Addr::new(d[0], d[1], d[2], d[3]))
    }

    fn secs(&self, code: u8) -> Option<Duration> {
        let d = self.opt(code)?;
        (d.len() >= 4)
            .then(|| Duration::from_secs(u32::from_be_bytes([d[0], d[1], d[2], d[3]]) as u64))
    }

    /// The lease an acknowledgement gives.
    fn lease(&self) -> Option<Lease> {
        let server = self.ip(OPT_SERVER)?;
        if self.yiaddr.is_unspecified() {
            return None;
        }
        let prefix = self
            .ip(OPT_MASK)
            .map(|m| u32::from(m).count_ones() as u8)
            .unwrap_or(24);
        // An infinite lease (all ones) is kept a day at a time.
        let time = self
            .secs(OPT_LEASE)
            .filter(|t| t.as_secs() < u32::MAX as u64)
            .unwrap_or(Duration::from_secs(86400))
            .max(Duration::from_secs(60));
        let renew = self.secs(OPT_RENEW).unwrap_or(time / 2).min(time);
        let rebind = self
            .secs(OPT_REBIND)
            .unwrap_or(time * 7 / 8)
            .clamp(renew, time);
        let dns = self
            .opt(OPT_DNS)
            .unwrap_or_default()
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| Ipv4Addr::from(*c))
            .collect();
        let domain = self
            .opt(OPT_DOMAIN)
            .map(|d| {
                String::from_utf8_lossy(d)
                    .trim_end_matches('\0')
                    .to_string()
            })
            .filter(|d| !d.is_empty());
        Some(Lease {
            address: self.yiaddr,
            prefix,
            router: self.ip(OPT_ROUTER),
            dns,
            domain,
            server,
            time,
            renew,
            rebind,
        })
    }
}

/// A server's answer to the client's transaction xid for mac, if b is one.
fn parse(b: &[u8], xid: u32, mac: &[u8; 6]) -> Option<Reply> {
    if b.len() < 240 || b[0] != 2 || b[4..8] != xid.to_be_bytes() || &b[28..34] != mac {
        return None;
    }
    if b[236..240] != COOKIE {
        return None;
    }
    let mut options: Vec<(u8, Vec<u8>)> = vec![];
    let mut i = 240;
    while i < b.len() {
        let code = b[i];
        if code == 0 {
            i += 1;
            continue;
        }
        if code == 255 || i + 1 >= b.len() {
            break;
        }
        let len = b[i + 1] as usize;
        let data = b.get(i + 2..i + 2 + len)?;
        // An option split over several is one (RFC 3396).
        match options.iter_mut().find(|(c, _)| *c == code) {
            Some((_, d)) => d.extend_from_slice(data),
            None => options.push((code, data.to_vec())),
        }
        i += 2 + len;
    }
    let kind = options
        .iter()
        .find(|(c, _)| *c == OPT_TYPE)?
        .1
        .first()
        .copied()?;
    Some(Reply {
        kind,
        yiaddr: Ipv4Addr::new(b[16], b[17], b[18], b[19]),
        options,
    })
}

fn xid() -> u32 {
    let mut b = [0u8; 4];
    let _ =
        std::fs::File::open("/dev/urandom").and_then(|mut f| io::Read::read_exact(&mut f, &mut b));
    u32::from_ne_bytes(b)
}

/// The machine's network card: the first Ethernet interface with a device
/// behind it (not dummy0 and its kind, which the kernel makes), once the
/// kernel has found one.
async fn card() -> String {
    loop {
        if let Ok(dir) = std::fs::read_dir("/sys/class/net") {
            let mut names: Vec<String> = dir
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| {
                    std::fs::read_to_string(format!("/sys/class/net/{n}/type"))
                        .is_ok_and(|t| t.trim() == "1")
                        && std::path::Path::new(&format!("/sys/class/net/{n}/device")).exists()
                })
                .collect();
            names.sort();
            if let Some(n) = names.into_iter().next() {
                return n;
            }
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

fn mac_of(iface: &str) -> io::Result<[u8; 6]> {
    let s = std::fs::read_to_string(format!("/sys/class/net/{iface}/address"))?;
    let mut mac = [0u8; 6];
    let parts: Vec<&str> = s.trim().split(':').collect();
    if parts.len() != 6 {
        return Err(io::Error::other(format!("no MAC: {s:?}")));
    }
    for (m, p) in mac.iter_mut().zip(parts) {
        *m = u8::from_str_radix(p, 16).map_err(io::Error::other)?;
    }
    Ok(mac)
}

/// A UDP socket on the client's port, on iface alone, that may broadcast.
fn socket(iface: &str) -> io::Result<UdpSocket> {
    // SAFETY: socket(2), owned at once.
    let fd = unsafe {
        libc::socket(
            libc::AF_INET,
            libc::SOCK_DGRAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
            0,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a descriptor socket(2) just returned, owned by no one else.
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    let one: libc::c_int = 1;
    for opt in [libc::SO_REUSEADDR, libc::SO_BROADCAST] {
        setsockopt(&fd, opt, &one.to_ne_bytes())?;
    }
    setsockopt(&fd, libc::SO_BINDTODEVICE, iface.as_bytes())?;
    let addr = sockaddr(Ipv4Addr::UNSPECIFIED, CLIENT_PORT);
    // SAFETY: addr is a sockaddr_in of the size given.
    let rc = unsafe {
        libc::bind(
            fd.as_raw_fd(),
            (&addr as *const libc::sockaddr_in).cast(),
            std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
        )
    };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    UdpSocket::from_std(std::net::UdpSocket::from(fd))
}

fn setsockopt(fd: &OwnedFd, opt: libc::c_int, value: &[u8]) -> io::Result<()> {
    // SAFETY: value is valid for its length.
    let rc = unsafe {
        libc::setsockopt(
            fd.as_raw_fd(),
            libc::SOL_SOCKET,
            opt,
            value.as_ptr().cast(),
            value.len() as libc::socklen_t,
        )
    };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn sockaddr(ip: Ipv4Addr, port: u16) -> libc::sockaddr_in {
    // SAFETY: an all-zero sockaddr_in is a valid one.
    let mut a: libc::sockaddr_in = unsafe { std::mem::zeroed() };
    a.sin_family = libc::AF_INET as libc::sa_family_t;
    a.sin_port = port.to_be();
    a.sin_addr.s_addr = u32::from(ip).to_be();
    a
}

/// A socket to ask the kernel about interfaces and routes with.
fn control() -> io::Result<OwnedFd> {
    // SAFETY: socket(2), owned at once.
    let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a descriptor socket(2) just returned, owned by no one else.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn ifreq(name: &str) -> libc::ifreq {
    // SAFETY: an all-zero ifreq is a valid one.
    let mut r: libc::ifreq = unsafe { std::mem::zeroed() };
    for (d, s) in r
        .ifr_name
        .iter_mut()
        .zip(name.bytes().take(libc::IFNAMSIZ - 1))
    {
        *d = s as libc::c_char;
    }
    r
}

/// An ioctl request on fd: ioctl's own type for requests differs between C
/// libraries, and the requests are all unsigned longs.
fn ioctl<T>(fd: &OwnedFd, req: libc::c_ulong, arg: &mut T) -> io::Result<()> {
    // SAFETY: arg is the structure req reads or fills.
    if unsafe { libc::ioctl(fd.as_raw_fd(), req as _, arg as *mut T) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Brings an interface up.
fn up(name: &str) -> io::Result<()> {
    let fd = control()?;
    let mut r = ifreq(name);
    ioctl(&fd, libc::SIOCGIFFLAGS, &mut r)?;
    // SAFETY: SIOCGIFFLAGS filled ifru_flags.
    unsafe { r.ifr_ifru.ifru_flags |= libc::IFF_UP as libc::c_short };
    ioctl(&fd, libc::SIOCSIFFLAGS, &mut r)
}

fn set_addr(fd: &OwnedFd, iface: &str, req: libc::c_ulong, ip: Ipv4Addr) -> io::Result<()> {
    let mut r = ifreq(iface);
    let a = sockaddr(ip, 0);
    // SAFETY: a sockaddr_in fits in the union's sockaddr, as the kernel
    // reads it for these requests.
    unsafe {
        std::ptr::copy_nonoverlapping(
            (&a as *const libc::sockaddr_in).cast::<u8>(),
            &mut r.ifr_ifru as *mut _ as *mut u8,
            std::mem::size_of::<libc::sockaddr_in>(),
        );
    }
    ioctl(fd, req, &mut r)
}

/// Puts a lease's address, netmask and default route on iface, in place of
/// old's, and its name servers in /etc/resolv.conf.
fn configure(iface: &str, l: &Lease, old: Option<&Lease>) -> io::Result<()> {
    let fd = control()?;
    if old.is_some() {
        set_addr(&fd, iface, libc::SIOCSIFADDR, Ipv4Addr::UNSPECIFIED)?;
    }
    set_addr(&fd, iface, libc::SIOCSIFADDR, l.address)?;
    let mask = Ipv4Addr::from(u32::MAX.checked_shl(32 - l.prefix as u32).unwrap_or(0));
    set_addr(&fd, iface, libc::SIOCSIFNETMASK, mask)?;
    if let Some(router) = l.router {
        // SAFETY: an all-zero rtentry is a valid one.
        let mut rt: libc::rtentry = unsafe { std::mem::zeroed() };
        let dev = std::ffi::CString::new(iface).map_err(io::Error::other)?;
        let put = |dst: &mut libc::sockaddr, ip: Ipv4Addr| {
            let a = sockaddr(ip, 0);
            // SAFETY: a sockaddr_in fits in a sockaddr.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    (&a as *const libc::sockaddr_in).cast::<u8>(),
                    (dst as *mut libc::sockaddr).cast::<u8>(),
                    std::mem::size_of::<libc::sockaddr_in>(),
                );
            }
        };
        put(&mut rt.rt_dst, Ipv4Addr::UNSPECIFIED);
        put(&mut rt.rt_gateway, router);
        put(&mut rt.rt_genmask, Ipv4Addr::UNSPECIFIED);
        rt.rt_flags = (libc::RTF_UP | libc::RTF_GATEWAY) as libc::c_ushort;
        rt.rt_dev = dev.as_ptr() as *mut libc::c_char;
        match ioctl(&fd, libc::SIOCADDRT, &mut rt) {
            Err(e) if e.raw_os_error() != Some(libc::EEXIST) => return Err(e),
            _ => {}
        }
    }
    let mut conf = String::new();
    if let Some(d) = &l.domain {
        conf.push_str(&format!("search {d}\n"));
    }
    for ns in &l.dns {
        conf.push_str(&format!("nameserver {ns}\n"));
    }
    std::fs::create_dir_all("/etc")?;
    std::fs::write("/etc/resolv.conf", conf)
}

/// Takes the address off iface, and with it the routes through it.
fn deconfigure(iface: &str) -> io::Result<()> {
    let fd = control()?;
    set_addr(&fd, iface, libc::SIOCSIFADDR, Ipv4Addr::UNSPECIFIED)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(kind: u8, xid: u32, mac: [u8; 6], options: &[(u8, &[u8])]) -> Vec<u8> {
        let mut b = vec![0u8; 236];
        b[0] = 2;
        b[4..8].copy_from_slice(&xid.to_be_bytes());
        b[16..20].copy_from_slice(&[192, 168, 1, 50]);
        b[28..34].copy_from_slice(&mac);
        b.extend_from_slice(&COOKIE);
        b.extend_from_slice(&[OPT_TYPE, 1, kind]);
        for (c, d) in options {
            b.push(*c);
            b.push(d.len() as u8);
            b.extend_from_slice(d);
        }
        b.push(255);
        b
    }

    #[test]
    fn an_acknowledgement_gives_its_lease() {
        let mac = [2, 0, 0, 0, 0, 1];
        let b = reply(
            ACK,
            7,
            mac,
            &[
                (OPT_SERVER, &[192, 168, 1, 1]),
                (OPT_MASK, &[255, 255, 255, 0]),
                (OPT_ROUTER, &[192, 168, 1, 1]),
                (OPT_DNS, &[192, 168, 1, 1, 1, 1, 1, 1]),
                (OPT_LEASE, &3600u32.to_be_bytes()),
            ],
        );
        let r = parse(&b, 7, &mac).expect("parsed");
        assert_eq!(r.kind, ACK);
        let l = r.lease().expect("a lease");
        assert_eq!(l.address, Ipv4Addr::new(192, 168, 1, 50));
        assert_eq!(l.prefix, 24);
        assert_eq!(l.router, Some(Ipv4Addr::new(192, 168, 1, 1)));
        assert_eq!(
            l.dns,
            vec![Ipv4Addr::new(192, 168, 1, 1), Ipv4Addr::new(1, 1, 1, 1)]
        );
        assert_eq!(l.renew, Duration::from_secs(1800));
        assert_eq!(l.rebind, Duration::from_secs(3150));
    }

    #[test]
    fn answers_to_others_are_not_ours() {
        let mac = [2, 0, 0, 0, 0, 1];
        let b = reply(OFFER, 7, mac, &[(OPT_SERVER, &[192, 168, 1, 1])]);
        assert!(parse(&b, 8, &mac).is_none());
        assert!(parse(&b, 7, &[2, 0, 0, 0, 0, 2]).is_none());
        assert!(parse(&b[..200], 7, &mac).is_none());
    }

    #[test]
    fn a_split_option_is_one() {
        let mac = [2, 0, 0, 0, 0, 1];
        let b = reply(
            ACK,
            1,
            mac,
            &[
                (OPT_SERVER, &[10, 0, 0, 1]),
                (OPT_DNS, &[10, 0, 0, 2]),
                (OPT_DNS, &[10, 0, 0, 3]),
            ],
        );
        let l = parse(&b, 1, &mac).and_then(|r| r.lease()).expect("a lease");
        assert_eq!(
            l.dns,
            vec![Ipv4Addr::new(10, 0, 0, 2), Ipv4Addr::new(10, 0, 0, 3)]
        );
    }
}
