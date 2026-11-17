//! The guest's own firewall, for a machine on a network others share (a
//! Proxmox VM on its place's bridge): nothing arriving from the network
//! starts a connection, since everything Hangar reaches in the guest comes
//! through the agent's own link out to its worker. Replies, DHCP and IPv6's
//! neighbour discovery come in; outbound is open. Traffic from Docker's
//! bridges, a VPN's tunnel or any other interface with a kind is the
//! guest's own and passes, as does forwarding out of them: so a container's
//! published port is not reachable from the network either.
//!
//! It is one nftables table, `inet hangar`, put in place by init before the
//! image's own programs run, through nf_tables' netlink interface (no `nft`
//! in the image is needed), and put back by the agent whenever it has gone:
//! an image's nftables.service starts with `flush ruleset`. Root in the
//! guest can stop the agent and remove it, which exposes only that
//! environment.
//!
//! A worker VM's firewall lets in the one port its hub listens on, which
//! its environments dial (`open_tcp`).

use std::io;
use std::time::Duration;

/// The table, replaced whole each time.
pub const TABLE: &str = "hangar";

const NETLINK_NETFILTER: i32 = 12;
const NFNL_SUBSYS_NFTABLES: u16 = 10;
const NFNL_MSG_BATCH_BEGIN: u16 = 16;
const NFNL_MSG_BATCH_END: u16 = 17;
const NFT_MSG_NEWTABLE: u16 = 0;
const NFT_MSG_GETTABLE: u16 = 1;
const NFT_MSG_NEWCHAIN: u16 = 3;
const NFT_MSG_NEWRULE: u16 = 6;
const NFT_MSG_DESTROYTABLE: u16 = 26;
const NFPROTO_INET: u8 = 1;

const NLM_F_REQUEST: u16 = 1;
const NLM_F_ACK: u16 = 4;
const NLM_F_CREATE: u16 = 0x400;
const NLM_F_APPEND: u16 = 0x800;
const NLMSG_ERROR: u16 = 2;
const NLA_F_NESTED: u16 = 0x8000;

const NFTA_TABLE_NAME: u16 = 1;
const NFTA_CHAIN_TABLE: u16 = 1;
const NFTA_CHAIN_NAME: u16 = 3;
const NFTA_CHAIN_HOOK: u16 = 4;
const NFTA_CHAIN_POLICY: u16 = 5;
const NFTA_CHAIN_TYPE: u16 = 7;
const NFTA_HOOK_HOOKNUM: u16 = 1;
const NFTA_HOOK_PRIORITY: u16 = 2;
const NFTA_RULE_TABLE: u16 = 1;
const NFTA_RULE_CHAIN: u16 = 2;
const NFTA_RULE_EXPRESSIONS: u16 = 4;
const NFTA_LIST_ELEM: u16 = 1;
const NFTA_EXPR_NAME: u16 = 1;
const NFTA_EXPR_DATA: u16 = 2;
const NFTA_DATA_VALUE: u16 = 1;
const NFTA_DATA_VERDICT: u16 = 2;
const NFTA_VERDICT_CODE: u16 = 1;

const NF_INET_LOCAL_IN: u32 = 1;
const NF_INET_FORWARD: u32 = 2;
const NF_DROP: u32 = 0;
const NF_ACCEPT: u32 = 1;

const NFT_REG_VERDICT: u32 = 0;
const NFT_REG_1: u32 = 1;
const NFT_META_IIFTYPE: u32 = 8;
const NFT_META_L4PROTO: u32 = 16;
const NFT_META_IIFKIND: u32 = 26;
const NFT_CT_STATE: u32 = 0;
const NFT_PAYLOAD_TRANSPORT_HEADER: u32 = 2;
const NFT_CMP_EQ: u32 = 0;
const NFT_CMP_NEQ: u32 = 1;

const ARPHRD_LOOPBACK: u16 = 772;
/// ct state established and related (NF_CT_STATE_BIT of each).
const CT_ESTABLISHED_RELATED: u32 = 2 | 4;
const IPPROTO_TCP: u8 = 6;
const IPPROTO_UDP: u8 = 17;
const IPPROTO_ICMPV6: u8 = 58;
const ICMPV6_ECHO_REQUEST: u8 = 128;
const IFNAMSIZ: usize = 16;

/// How often the agent looks for the firewall.
const KEEP_EVERY: Duration = Duration::from_secs(5);

/// Whether the kernel command line asks for the firewall
/// (`hangar.firewall=1`), as the worker does for a machine on a shared
/// network.
pub fn wanted() -> bool {
    std::fs::read_to_string("/proc/cmdline")
        .unwrap_or_default()
        .split_whitespace()
        .filter_map(|a| a.strip_prefix("hangar.firewall="))
        .next_back()
        .is_some_and(|v| v != "0")
}

/// The TCP ports connections may be made to from the network.
fn open() -> &'static std::sync::Mutex<Vec<u16>> {
    static OPEN: std::sync::OnceLock<std::sync::Mutex<Vec<u16>>> = std::sync::OnceLock::new();
    OPEN.get_or_init(Default::default)
}

/// Lets connections to port in from the network, as a worker VM's hub
/// needs; from the next install.
pub fn open_tcp(port: u16) {
    let mut o = open().lock().unwrap();
    if !o.contains(&port) {
        o.push(port);
    }
}

/// Puts the firewall back whenever it has gone, for as long as the agent
/// runs.
pub async fn keep() {
    loop {
        match tokio::task::spawn_blocking(installed).await {
            Ok(Ok(true)) => {}
            Ok(Ok(false)) => match tokio::task::spawn_blocking(install).await {
                Ok(Ok(())) => eprintln!("hangar-agent: firewall: it had gone; put it back"),
                Ok(Err(e)) => eprintln!("hangar-agent: firewall: putting it back: {e}"),
                Err(_) => {}
            },
            Ok(Err(e)) => eprintln!("hangar-agent: firewall: looking for it: {e}"),
            Err(_) => {}
        }
        tokio::time::sleep(KEEP_EVERY).await;
    }
}

/// Whether the firewall's table is there.
pub fn installed() -> io::Result<bool> {
    let mut get = Batch::default();
    get.table(NFT_MSG_GETTABLE, 0);
    match exchange(&get.msgs, get.acked)?.first() {
        Some((_, 0)) => Ok(true),
        Some((_, code)) if -code == libc::ENOENT => Ok(false),
        Some((_, code)) => Err(io::Error::from_raw_os_error(-code)),
        None => Err(io::Error::other("nf_tables did not answer")),
    }
}

/// Puts the firewall in place, replacing any table of its name, in one
/// transaction: the guest is never without it once this returns.
pub fn install() -> io::Result<()> {
    let mut batch = Batch::default();
    batch.table(NFT_MSG_DESTROYTABLE, 0);
    batch.table(NFT_MSG_NEWTABLE, NLM_F_CREATE);
    for (chain, hook) in [("input", NF_INET_LOCAL_IN), ("forward", NF_INET_FORWARD)] {
        batch.chain(chain, hook, NF_DROP);
        batch.rule(chain, &[Expr::Kind, accept()]);
        batch.rule(chain, &established());
    }
    batch.rule(
        "input",
        &[
            Expr::LoadIfType,
            eq(&ARPHRD_LOOPBACK.to_ne_bytes()),
            accept(),
        ],
    );
    for port in open().lock().unwrap().iter() {
        batch.rule(
            "input",
            &[
                Expr::LoadL4Proto,
                eq(&[IPPROTO_TCP]),
                Expr::LoadTransport { offset: 2, len: 2 },
                eq(&port.to_be_bytes()),
                accept(),
            ],
        );
    }
    for port in [68u16, 546] {
        batch.rule(
            "input",
            &[
                Expr::LoadL4Proto,
                eq(&[IPPROTO_UDP]),
                Expr::LoadTransport { offset: 2, len: 2 },
                eq(&port.to_be_bytes()),
                accept(),
            ],
        );
    }
    // Neighbour discovery, router advertisements and multicast listening,
    // without which IPv6 does not work; echo requests stay out.
    batch.rule(
        "input",
        &[
            Expr::LoadL4Proto,
            eq(&[IPPROTO_ICMPV6]),
            Expr::LoadTransport { offset: 0, len: 1 },
            Expr::Cmp {
                op: NFT_CMP_NEQ,
                data: vec![ICMPV6_ECHO_REQUEST],
            },
            accept(),
        ],
    );
    batch.send()
}

/// Accepts a packet of a connection the guest made, or one related to it
/// (an ICMP error about it).
fn established() -> [Expr; 4] {
    [
        Expr::LoadCtState,
        Expr::Mask(CT_ESTABLISHED_RELATED.to_ne_bytes().to_vec()),
        Expr::Cmp {
            op: NFT_CMP_NEQ,
            data: vec![0; 4],
        },
        accept(),
    ]
}

fn accept() -> Expr {
    Expr::Verdict(NF_ACCEPT)
}

fn eq(data: &[u8]) -> Expr {
    Expr::Cmp {
        op: NFT_CMP_EQ,
        data: data.to_vec(),
    }
}

/// An nftables expression, each loading into or reading register 1.
enum Expr {
    /// Whether the input interface has a kind: a bridge, a veth, a tunnel,
    /// anything the guest made, as against the machine's own network card
    /// and loopback, which have none. A rule with it goes no further for
    /// an interface without one.
    Kind,
    LoadIfType,
    LoadL4Proto,
    LoadCtState,
    LoadTransport {
        offset: u32,
        len: u32,
    },
    /// Register 1 ANDed with the mask.
    Mask(Vec<u8>),
    Cmp {
        op: u32,
        data: Vec<u8>,
    },
    Verdict(u32),
}

impl Expr {
    fn put(&self, b: &mut Attrs) {
        let meta = |b: &mut Attrs, key: u32| {
            b.nest(NFTA_EXPR_DATA, |b| {
                b.u32(1, NFT_REG_1); // NFTA_META_DREG
                b.u32(2, key); // NFTA_META_KEY
            })
        };
        let cmp = |b: &mut Attrs, op: u32, data: &[u8]| {
            b.str(NFTA_EXPR_NAME, "cmp");
            b.nest(NFTA_EXPR_DATA, |b| {
                b.u32(1, NFT_REG_1); // NFTA_CMP_SREG
                b.u32(2, op); // NFTA_CMP_OP
                b.nest(3, |b| b.attr(NFTA_DATA_VALUE, data)); // NFTA_CMP_DATA
            });
        };
        match self {
            Expr::Kind => {
                b.nest(NFTA_LIST_ELEM, |b| {
                    b.str(NFTA_EXPR_NAME, "meta");
                    meta(b, NFT_META_IIFKIND);
                });
                b.nest(NFTA_LIST_ELEM, |b| {
                    cmp(b, NFT_CMP_NEQ, &[0; IFNAMSIZ]);
                });
            }
            Expr::LoadIfType | Expr::LoadL4Proto => b.nest(NFTA_LIST_ELEM, |b| {
                b.str(NFTA_EXPR_NAME, "meta");
                let key = match self {
                    Expr::LoadIfType => NFT_META_IIFTYPE,
                    _ => NFT_META_L4PROTO,
                };
                meta(b, key);
            }),
            Expr::LoadCtState => b.nest(NFTA_LIST_ELEM, |b| {
                b.str(NFTA_EXPR_NAME, "ct");
                b.nest(NFTA_EXPR_DATA, |b| {
                    b.u32(1, NFT_REG_1); // NFTA_CT_DREG
                    b.u32(2, NFT_CT_STATE); // NFTA_CT_KEY
                });
            }),
            Expr::LoadTransport { offset, len } => b.nest(NFTA_LIST_ELEM, |b| {
                b.str(NFTA_EXPR_NAME, "payload");
                b.nest(NFTA_EXPR_DATA, |b| {
                    b.u32(1, NFT_REG_1); // NFTA_PAYLOAD_DREG
                    b.u32(2, NFT_PAYLOAD_TRANSPORT_HEADER); // NFTA_PAYLOAD_BASE
                    b.u32(3, *offset); // NFTA_PAYLOAD_OFFSET
                    b.u32(4, *len); // NFTA_PAYLOAD_LEN
                });
            }),
            Expr::Mask(mask) => b.nest(NFTA_LIST_ELEM, |b| {
                b.str(NFTA_EXPR_NAME, "bitwise");
                b.nest(NFTA_EXPR_DATA, |b| {
                    b.u32(1, NFT_REG_1); // NFTA_BITWISE_SREG
                    b.u32(2, NFT_REG_1); // NFTA_BITWISE_DREG
                    b.u32(3, mask.len() as u32); // NFTA_BITWISE_LEN
                    b.nest(4, |b| b.attr(NFTA_DATA_VALUE, mask)); // NFTA_BITWISE_MASK
                    b.nest(5, |b| b.attr(NFTA_DATA_VALUE, &vec![0; mask.len()])); // XOR
                });
            }),
            Expr::Cmp { op, data } => b.nest(NFTA_LIST_ELEM, |b| cmp(b, *op, data)),
            Expr::Verdict(code) => b.nest(NFTA_LIST_ELEM, |b| {
                b.str(NFTA_EXPR_NAME, "immediate");
                b.nest(NFTA_EXPR_DATA, |b| {
                    b.u32(1, NFT_REG_VERDICT); // NFTA_IMMEDIATE_DREG
                    b.nest(2, |b| {
                        // NFTA_IMMEDIATE_DATA
                        b.nest(NFTA_DATA_VERDICT, |b| b.u32(NFTA_VERDICT_CODE, *code));
                    });
                });
            }),
        }
    }
}

/// Netlink attributes, appended to a message.
#[derive(Default)]
struct Attrs(Vec<u8>);

impl Attrs {
    fn attr(&mut self, ty: u16, data: &[u8]) {
        let len = 4 + data.len();
        self.0.extend_from_slice(&(len as u16).to_ne_bytes());
        self.0.extend_from_slice(&ty.to_ne_bytes());
        self.0.extend_from_slice(data);
        self.pad();
    }

    fn str(&mut self, ty: u16, s: &str) {
        let mut b = s.as_bytes().to_vec();
        b.push(0);
        self.attr(ty, &b);
    }

    /// nf_tables' numbers are big-endian.
    fn u32(&mut self, ty: u16, v: u32) {
        self.attr(ty, &v.to_be_bytes());
    }

    fn nest(&mut self, ty: u16, f: impl FnOnce(&mut Attrs)) {
        let start = self.0.len();
        self.0.extend_from_slice(&[0; 4]);
        f(self);
        let len = (self.0.len() - start) as u16;
        self.0[start..start + 2].copy_from_slice(&len.to_ne_bytes());
        self.0[start + 2..start + 4].copy_from_slice(&(ty | NLA_F_NESTED).to_ne_bytes());
    }

    fn pad(&mut self) {
        while !self.0.len().is_multiple_of(4) {
            self.0.push(0);
        }
    }
}

/// One nf_tables transaction: its messages between a batch's beginning
/// and end, each acknowledged.
#[derive(Default)]
struct Batch {
    msgs: Vec<u8>,
    acked: u32,
}

impl Batch {
    fn message(&mut self, ty: u16, flags: u16, family: u8, res_id: u16, attrs: &Attrs) {
        let seq = self.acked + 1;
        let len = 16 + 4 + attrs.0.len();
        let m = &mut self.msgs;
        m.extend_from_slice(&(len as u32).to_ne_bytes());
        m.extend_from_slice(&ty.to_ne_bytes());
        m.extend_from_slice(&(NLM_F_REQUEST | flags).to_ne_bytes());
        m.extend_from_slice(&seq.to_ne_bytes());
        m.extend_from_slice(&0u32.to_ne_bytes());
        m.push(family);
        m.push(0); // NFNETLINK_V0
        m.extend_from_slice(&res_id.to_be_bytes());
        m.extend_from_slice(&attrs.0);
        if flags & NLM_F_ACK != 0 {
            self.acked += 1;
        }
    }

    fn nft(&mut self, ty: u8, flags: u16, attrs: &Attrs) {
        self.message(
            (NFNL_SUBSYS_NFTABLES << 8) | ty as u16,
            flags | NLM_F_ACK,
            NFPROTO_INET,
            0,
            attrs,
        );
    }

    fn table(&mut self, ty: u16, flags: u16) {
        let mut a = Attrs::default();
        a.str(NFTA_TABLE_NAME, TABLE);
        self.nft(ty as u8, flags, &a);
    }

    fn chain(&mut self, name: &str, hook: u32, policy: u32) {
        let mut a = Attrs::default();
        a.str(NFTA_CHAIN_TABLE, TABLE);
        a.str(NFTA_CHAIN_NAME, name);
        a.nest(NFTA_CHAIN_HOOK, |a| {
            a.u32(NFTA_HOOK_HOOKNUM, hook);
            a.u32(NFTA_HOOK_PRIORITY, 0);
        });
        a.u32(NFTA_CHAIN_POLICY, policy);
        a.str(NFTA_CHAIN_TYPE, "filter");
        self.nft(NFT_MSG_NEWCHAIN as u8, NLM_F_CREATE, &a);
    }

    fn rule(&mut self, chain: &str, exprs: &[Expr]) {
        let mut a = Attrs::default();
        a.str(NFTA_RULE_TABLE, TABLE);
        a.str(NFTA_RULE_CHAIN, chain);
        a.nest(NFTA_RULE_EXPRESSIONS, |a| {
            for e in exprs {
                e.put(a);
            }
        });
        self.nft(NFT_MSG_NEWRULE as u8, NLM_F_CREATE | NLM_F_APPEND, &a);
    }

    /// Sends the batch and waits for every message's answer: the first
    /// error, if any, is the batch's (the kernel then applies none of it).
    fn send(self) -> io::Result<()> {
        let mut out = Vec::new();
        let mut begin = Batch::default();
        begin.message(
            NFNL_MSG_BATCH_BEGIN,
            0,
            0,
            NFNL_SUBSYS_NFTABLES,
            &Attrs::default(),
        );
        out.extend_from_slice(&begin.msgs);
        out.extend_from_slice(&self.msgs);
        let mut end = Batch::default();
        end.message(
            NFNL_MSG_BATCH_END,
            0,
            0,
            NFNL_SUBSYS_NFTABLES,
            &Attrs::default(),
        );
        out.extend_from_slice(&end.msgs);
        match exchange(&out, self.acked)?
            .into_iter()
            .find(|(_, code)| *code != 0)
        {
            Some((seq, code)) => {
                let e = io::Error::from_raw_os_error(-code);
                Err(io::Error::new(
                    e.kind(),
                    format!("nf_tables refused message {seq} of the firewall: {e}"),
                ))
            }
            None => Ok(()),
        }
    }
}

/// Sends messages to nf_tables and gives the answers to the first
/// `answers` of them that asked for one: each one's sequence number and
/// error (0 for none).
fn exchange(out: &[u8], answers: u32) -> io::Result<Vec<(u32, i32)>> {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

    // SAFETY: socket(2); the descriptor is owned from here.
    let fd = unsafe {
        libc::socket(
            libc::AF_NETLINK,
            libc::SOCK_RAW | libc::SOCK_CLOEXEC,
            NETLINK_NETFILTER,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fd is a socket just made and owned by nothing else.
    let sock = unsafe { OwnedFd::from_raw_fd(fd) };
    // An answer that never comes fails the call rather than leaving init
    // waiting.
    let within = libc::timeval {
        tv_sec: 5,
        tv_usec: 0,
    };
    // SAFETY: setsockopt(2) with a timeval.
    unsafe {
        libc::setsockopt(
            sock.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_RCVTIMEO,
            (&within as *const libc::timeval).cast(),
            std::mem::size_of::<libc::timeval>() as u32,
        )
    };
    // SAFETY: sockaddr_nl is plain data; zeroed is the kernel's address.
    let mut kernel: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
    kernel.nl_family = libc::AF_NETLINK as u16;
    // SAFETY: sendto(2) of out, to the kernel's address.
    let sent = unsafe {
        libc::sendto(
            sock.as_raw_fd(),
            out.as_ptr().cast(),
            out.len(),
            0,
            (&kernel as *const libc::sockaddr_nl).cast(),
            std::mem::size_of::<libc::sockaddr_nl>() as u32,
        )
    };
    if sent < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut got = Vec::new();
    let mut buf = vec![0u8; 64 * 1024];
    while (got.len() as u32) < answers {
        // SAFETY: recv(2) into buf.
        let n = unsafe { libc::recv(sock.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len(), 0) };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        let mut at = 0;
        let n = n as usize;
        while at + 16 <= n {
            let len = u32::from_ne_bytes(buf[at..at + 4].try_into().unwrap()) as usize;
            let ty = u16::from_ne_bytes(buf[at + 4..at + 6].try_into().unwrap());
            if len < 16 || at + len > n {
                break;
            }
            if ty == NLMSG_ERROR && len >= 20 {
                let seq = u32::from_ne_bytes(buf[at + 8..at + 12].try_into().unwrap());
                let code = i32::from_ne_bytes(buf[at + 16..at + 20].try_into().unwrap());
                got.push((seq, code));
            }
            at += (len + 3) & !3;
        }
    }
    Ok(got)
}
