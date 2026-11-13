//! ONC RPC (RFC 5531), as NFS carries it: calls and replies on a stream,
//! each a record of one or more fragments (record marking).

use std::io::{self, Read, Write};

use crate::proto::MAX_IO;
use crate::xdr::{Reader, Writer};

const RPC_CALL: u32 = 0;
const RPC_REPLY: u32 = 1;
const RPC_VERSION: u32 = 2;
const NFS_PROGRAM: u32 = 100003;
const NFS_VERSION: u32 = 4;

pub const PROC_NULL: u32 = 0;
pub const PROC_COMPOUND: u32 = 1;

const AUTH_NONE: u32 = 0;
const AUTH_SYS: u32 = 1;

const MSG_ACCEPTED: u32 = 0;
const MSG_DENIED: u32 = 1;

pub const ACCEPT_SUCCESS: u32 = 0;
const ACCEPT_PROG_UNAVAIL: u32 = 1;
const ACCEPT_PROG_MISMATCH: u32 = 2;
const ACCEPT_PROC_UNAVAIL: u32 = 3;
pub const ACCEPT_GARBAGE_ARGS: u32 = 4;

const REJECT_RPC_MISMATCH: u32 = 0;

const LAST_FRAGMENT: u32 = 1 << 31;

/// Bounds a call: a WRITE of MAX_IO and its compound around it.
const MAX_RECORD: usize = MAX_IO as usize + (64 << 10);

/// Reads one record, its fragments joined. None at the end of the stream.
pub fn read_record(r: &mut impl Read) -> io::Result<Option<Vec<u8>>> {
    let mut rec = vec![];
    loop {
        let mut hdr = [0u8; 4];
        match r.read_exact(&mut hdr) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof && rec.is_empty() => {
                return Ok(None);
            }
            Err(e) => return Err(e),
        }
        let h = u32::from_be_bytes(hdr);
        let n = (h & !LAST_FRAGMENT) as usize;
        if rec.len() + n > MAX_RECORD {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("nfs: a record of more than {MAX_RECORD} bytes"),
            ));
        }
        let start = rec.len();
        rec.resize(start + n, 0);
        r.read_exact(&mut rec[start..])?;
        if h & LAST_FRAGMENT != 0 {
            return Ok(Some(rec));
        }
    }
}

/// Writes b as one record of one fragment.
pub fn write_record(w: &mut impl Write, b: &[u8]) -> io::Result<()> {
    let mut out = Vec::with_capacity(4 + b.len());
    out.extend_from_slice(&(LAST_FRAGMENT | b.len() as u32).to_be_bytes());
    out.extend_from_slice(b);
    w.write_all(&out)
}

/// Who a call is from, as AUTH_SYS says: the client's word, which is all
/// NFS has to go on. One with AUTH_NONE is nobody.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cred {
    pub uid: u32,
    pub gid: u32,
    pub gids: Vec<u32>,
}

pub const NOBODY: Cred = Cred {
    uid: 65534,
    gid: 65534,
    gids: vec![],
};

/// Why a call this server does not serve is refused, unmade.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reject {
    RpcMismatch,
    ProgUnavail,
    ProgMismatch,
    ProcUnavail,
}

/// An RPC call's header, and what follows it.
pub struct Call<'a> {
    pub xid: u32,
    pub proc_: u32,
    pub cred: Cred,
    pub args: &'a [u8],
    pub reject: Option<Reject>,
}

/// Reads a call's header. A call to anything but NFSv4 is to be answered
/// with its reject.
pub fn parse_call(b: &[u8]) -> Result<Call<'_>, &'static str> {
    let mut r = Reader::new(b);
    let xid = r.u32();
    if r.u32() != RPC_CALL {
        return Err("nfs: not a call");
    }
    let (rpcvers, prog, vers) = (r.u32(), r.u32(), r.u32());
    let proc_ = r.u32();
    let flavor = r.u32();
    let body = r.opaque(400);
    r.u32(); // the verifier's flavor
    r.opaque(400);
    if r.failed {
        return Err("nfs: malformed XDR");
    }
    let mut cred = NOBODY;
    if flavor == AUTH_SYS {
        let mut a = Reader::new(body);
        a.u32(); // stamp
        a.opaque(255); // machine name
        let (uid, gid) = (a.u32(), a.u32());
        let n = a.count(16);
        let gids: Vec<u32> = (0..n).map(|_| a.u32()).collect();
        if !a.failed {
            cred = Cred { uid, gid, gids };
        }
    }
    let reject = if rpcvers != RPC_VERSION {
        Some(Reject::RpcMismatch)
    } else if prog != NFS_PROGRAM {
        Some(Reject::ProgUnavail)
    } else if vers != NFS_VERSION {
        Some(Reject::ProgMismatch)
    } else if proc_ != PROC_NULL && proc_ != PROC_COMPOUND {
        Some(Reject::ProcUnavail)
    } else {
        None
    };
    Ok(Call {
        xid,
        proc_,
        cred,
        args: r.b,
        reject,
    })
}

/// Writes a rejected call's reply after its header.
pub fn reject(w: &mut Writer, why: Reject) {
    match why {
        Reject::RpcMismatch => {
            w.u32(MSG_DENIED);
            w.u32(REJECT_RPC_MISMATCH);
            w.u32(RPC_VERSION);
            w.u32(RPC_VERSION);
        }
        Reject::ProgUnavail => accepted(w, ACCEPT_PROG_UNAVAIL),
        Reject::ProgMismatch => {
            accepted(w, ACCEPT_PROG_MISMATCH);
            w.u32(NFS_VERSION);
            w.u32(NFS_VERSION);
        }
        Reject::ProcUnavail => accepted(w, ACCEPT_PROC_UNAVAIL),
    }
}

/// Starts a reply to xid.
pub fn reply_header(xid: u32) -> Writer {
    let mut w = Writer::default();
    w.u32(xid);
    w.u32(RPC_REPLY);
    w
}

/// Writes an accepted reply's status, with no verifier.
pub fn accepted(w: &mut Writer, stat: u32) {
    w.u32(MSG_ACCEPTED);
    w.u32(AUTH_NONE);
    w.u32(0);
    w.u32(stat);
}
