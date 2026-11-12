//! The user's SSH agent, read-only: each key it lists is one of the
//! profile's, and each signature is the server's. Private keys never enter
//! a guest.
//!
//! The protocol is OpenSSH's (draft-ietf-sshm-ssh-agent): each message a
//! four-byte length and that many bytes, the first its type.

use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

use super::files::Files;
use super::{Guest, Message, TYPE_SIGN};

const FAILURE: u8 = 5;
const REQUEST_IDENTITIES: u8 = 11;
const IDENTITIES_ANSWER: u8 = 12;
const SIGN_REQUEST: u8 = 13;
const SIGN_RESPONSE: u8 = 14;

/// The largest message taken, as OpenSSH's own agent.
const MAX_MESSAGE: usize = 256 << 10;

/// How long the agent waits for a session before listing the keys it has,
/// which with no session is none, or giving up on a signature.
const KEYS_WAIT: Duration = Duration::from_secs(30);

/// The wire form of the public key in an authorized-keys line: the field
/// after its type, decoded, when it names that type itself. Options before
/// the type are passed over.
pub fn parse_authorized_key(line: &str) -> Option<Vec<u8>> {
    let fields: Vec<&str> = line.split_whitespace().collect();
    fields.windows(2).find_map(|w| {
        let blob = base64::engine::general_purpose::STANDARD
            .decode(w[1])
            .ok()?;
        let mut r = Reader(&blob);
        (r.string()? == w[0].as_bytes()).then_some(blob)
    })
}

impl<F: Files> Guest<F> {
    /// Serves the SSH agent on `socket`, for the user, until it cannot.
    pub async fn serve_ssh_agent(self: &Arc<Self>, socket: &Path) -> io::Result<()> {
        if let Some(dir) = socket.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let _ = std::fs::remove_file(socket);
        let ln = UnixListener::bind(socket)?;
        std::os::unix::fs::lchown(socket, Some(self.user.uid), Some(self.user.gid))?;
        std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))?;
        loop {
            let (conn, _) = ln.accept().await?;
            let g = self.clone();
            tokio::spawn(async move { g.agent_conn(conn).await });
        }
    }

    async fn agent_conn(&self, mut conn: UnixStream) {
        loop {
            let mut len = [0; 4];
            if conn.read_exact(&mut len).await.is_err() {
                return;
            }
            let n = u32::from_be_bytes(len) as usize;
            if n == 0 || n > MAX_MESSAGE {
                return;
            }
            let mut req = vec![0; n];
            if conn.read_exact(&mut req).await.is_err() {
                return;
            }
            let reply = self.agent_request(&req).await;
            let mut out = (reply.len() as u32).to_be_bytes().to_vec();
            out.extend_from_slice(&reply);
            if conn.write_all(&out).await.is_err() {
                return;
            }
        }
    }

    /// Answers one request: the keys, or a signature. Anything else would
    /// change the keys, which are managed in Hangar, and fails.
    async fn agent_request(&self, req: &[u8]) -> Vec<u8> {
        match req[0] {
            REQUEST_IDENTITIES => {
                // A clone made while the environment is still starting asks
                // before the profile session has connected. The keys come
                // before the profile's files, so a large profile does not
                // hold them up.
                self.await_session(KEYS_WAIT, |s| s.keys_known()).await;
                let keys = self.state.lock().unwrap().keys.clone();
                let mut out = vec![IDENTITIES_ANSWER];
                out.extend_from_slice(&(keys.len() as u32).to_be_bytes());
                for k in keys {
                    put_string(&mut out, &k);
                    put_string(&mut out, b"hangar");
                }
                out
            }
            SIGN_REQUEST => {
                let mut r = Reader(&req[1..]);
                let (Some(key), Some(data), Some(flags)) = (r.string(), r.string(), r.u32()) else {
                    return vec![FAILURE];
                };
                let Some(s) = self.connected(KEYS_WAIT).await else {
                    return vec![FAILURE];
                };
                let ask = Message {
                    kind: TYPE_SIGN.into(),
                    key: key.to_vec(),
                    data: data.to_vec(),
                    flags,
                    ..Message::default()
                };
                match s.ask(ask).await {
                    // A signature is its format and blob.
                    Ok(m) if Reader(&m.signature).string().is_some() => {
                        let mut out = vec![SIGN_RESPONSE];
                        put_string(&mut out, &m.signature);
                        out
                    }
                    _ => vec![FAILURE],
                }
            }
            _ => vec![FAILURE],
        }
    }
}

fn put_string(out: &mut Vec<u8>, s: &[u8]) {
    out.extend_from_slice(&(s.len() as u32).to_be_bytes());
    out.extend_from_slice(s);
}

/// Reads the protocol's strings and integers.
struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn u32(&mut self) -> Option<u32> {
        let (n, rest) = self.0.split_first_chunk::<4>()?;
        self.0 = rest;
        Some(u32::from_be_bytes(*n))
    }

    fn string(&mut self) -> Option<&'a [u8]> {
        let n = self.u32()? as usize;
        if self.0.len() < n {
            return None;
        }
        let (s, rest) = self.0.split_at(n);
        self.0 = rest;
        Some(s)
    }
}
