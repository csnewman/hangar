//! Hangar's SSH gateway: one SSH server in front of every environment.
//!
//!     ssh prof-a@hangar.example.com -p 2222        one of your environments
//!     ssh alice/prof-a@hangar.example.com -p 2222  someone else's you may reach
//!
//! SSH has nothing like HTTP's Host header, so the environment is named in
//! the username, and the person is known by their key: one of their
//! sign-in keys. The gateway ends the connection's encryption to read
//! both, and carries each channel the client opens -- shells, commands,
//! local port forwards -- to the SSH server in the environment's agent,
//! over the worker's tunnel. Every connection is in the audit log.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use russh::keys::PublicKeyOrCertificate;
use russh::keys::ssh_key::PublicKey;
use russh::server::{
    self, Auth, ChannelOpenHandle, Msg, PublicKeyOffer, Session, VerifiedPublicKey,
};
use russh::{Channel, ChannelId, ChannelMsg, ChannelOpenFailure, Pty, Sig};
use tokio::sync::oneshot;
use uuid::Uuid;

use crate::audit::Actor;
use crate::environments::{self as envs, View};
use crate::http::Shared;
use crate::store::rows::User;
use crate::tunnel::{self, Header};
use crate::users::Principal;

/// How often each client is asked for an answer, so an idle connection
/// stays through NATs and proxies; one that has not answered three times
/// is gone.
const KEEPALIVE: Duration = Duration::from_secs(30);

/// How long the guest has to answer a request.
const ANSWER_WITHIN: Duration = Duration::from_secs(20);

/// Serves SSH on addr until the process ends.
pub async fn serve(app: Shared, addr: String, key: russh::keys::PrivateKey) {
    let config = Arc::new(server::Config {
        server_id: russh::SshId::Standard("SSH-2.0-hangar".into()),
        methods: russh::MethodSet::from(&[russh::MethodKind::PublicKey][..]),
        keys: vec![key],
        keepalive_interval: Some(KEEPALIVE),
        keepalive_max: 3,
        // An idle shell is the person's to end.
        inactivity_timeout: None,
        auth_rejection_time: Duration::from_millis(250),
        auth_rejection_time_initial: Some(Duration::ZERO),
        nodelay: true,
        ..server::Config::default()
    });
    let listener = match crate::listen::tcp(&addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("hangar-server: serving SSH on {addr}: {e}");
            return;
        }
    };
    eprintln!("hangar-server: serving SSH on {addr}");
    loop {
        let Ok((tcp, peer)) = listener.accept().await else {
            continue;
        };
        let (app, config) = (app.clone(), config.clone());
        tokio::spawn(async move {
            let conn = Conn {
                app,
                ip: peer.ip().to_string(),
                found: None,
                up: None,
                refused: None,
                started: Instant::now(),
                client: String::new(),
                channels: HashMap::new(),
            };
            let handshake = tokio::time::timeout(
                Duration::from_secs(30),
                server::run_stream(config, tcp, conn),
            );
            if let Ok(Ok(running)) = handshake.await {
                let _ = running.await;
            }
        });
    }
}

/// Who signed in, and the environment they named.
#[derive(Clone)]
struct Found {
    person: User,
    env: Uuid,
    fingerprint: String,
}

/// One connection.
struct Conn {
    app: Shared,
    ip: String,
    found: Option<Found>,
    /// The guest's SSH server, once dialled.
    up: Option<russh::client::Handle<Up>>,
    /// Why the environment cannot be reached, when it cannot.
    refused: Option<String>,
    started: Instant,
    client: String,
    channels: HashMap<ChannelId, Upstream>,
}

/// A channel's guest half: where its requests go, and the answers they
/// are waiting for, in order.
struct Upstream {
    write: Arc<russh::ChannelWriteHalf<russh::client::Msg>>,
    answers: Arc<Mutex<VecDeque<oneshot::Sender<bool>>>>,
}

/// The guest's side of the connection: its host key has nothing to prove,
/// since it is reached only through the worker's tunnel, which is the
/// server's own; and it opens no channels of its own.
struct Up;

impl russh::client::Handler for Up {
    type Error = russh::Error;

    async fn check_server_key(&mut self, _: &PublicKeyOrCertificate) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

impl Conn {
    fn principal(&self) -> Option<Principal> {
        self.found.as_ref().map(|f| Principal {
            user_id: f.person.id,
            username: f.person.username.clone(),
            admin: f.person.is_admin,
            via: format!("ssh key {}", f.fingerprint),
        })
    }

    fn actor(&self) -> Actor {
        self.principal()
            .map(|p| p.actor(&self.ip))
            .unwrap_or_default()
    }

    /// The environment the username names that key's owners may reach:
    /// "name" for one of their own, "owner/name" otherwise, or a short ID,
    /// which names one on the whole server. A name of the owner's own is
    /// taken first, so what someone types means what it always did.
    async fn find(&self, user: &str, key: &PublicKey) -> Option<Found> {
        let owners = match crate::keys::login_key_owners(&self.app.store, key).await {
            Ok(o) => o,
            Err(e) => {
                eprintln!("hangar-server: ssh: looking up a key: {e}");
                return None;
            }
        };
        let (owner, name) = match user.split_once('/') {
            Some((o, n)) => (Some(o), n),
            None => (None, user),
        };
        let short = name.len() == 6
            && name
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
        for person in owners {
            let p = Principal {
                user_id: person.id,
                username: person.username.clone(),
                admin: person.is_admin,
                via: String::new(),
            };
            let list = match envs::list(&self.app.store, &p).await {
                Ok(l) => l,
                Err(e) => {
                    eprintln!("hangar-server: ssh: looking up an environment: {e}");
                    return None;
                }
            };
            let live = list.iter().filter(|v| v.row.desired != "deleted");
            let who = owner.unwrap_or(&person.username).to_lowercase();
            let mut hit = live.clone().find(|v| {
                v.owner.to_lowercase() == who && v.row.name.to_lowercase() == name.to_lowercase()
            });
            if hit.is_none() && owner.is_none() && short {
                hit = live.clone().find(|v| v.row.short_id == name);
            }
            if let Some(v) = hit {
                return Some(Found {
                    env: v.row.id,
                    fingerprint: crate::keys::fingerprint(key),
                    person,
                });
            }
        }
        None
    }

    async fn view(&self) -> Option<View> {
        let p = self.principal()?;
        let env = self.found.as_ref()?.env;
        envs::get(&self.app.store, &p, env).await.ok().flatten()
    }

    /// Connects to the environment's SSH server, through its worker.
    async fn dial(&self, v: &View) -> Result<russh::client::Handle<Up>, String> {
        let Some(worker) = v.row.worker_id.filter(|_| v.row.phase == "running") else {
            return Err(format!(
                "{} is {}, not running: start it first",
                v.row.name, v.row.phase
            ));
        };
        let env = v.row.id.to_string();
        let stream = self
            .app
            .tunnels
            .open(
                worker,
                &Header {
                    kind: tunnel::KIND_SSH,
                    environment: &env,
                },
            )
            .await
            .map_err(|_| {
                "the environment's worker cannot be reached from this server just now".to_string()
            })?;
        let config = Arc::new(russh::client::Config {
            nodelay: true,
            ..russh::client::Config::default()
        });
        let connect = russh::client::connect_stream(config, stream, Up);
        let mut up = match tokio::time::timeout(ANSWER_WITHIN, connect).await {
            Ok(Ok(h)) => h,
            Ok(Err(e)) => return Err(format!("the environment's SSH server did not answer: {e}")),
            Err(_) => return Err("the environment's SSH server did not answer".into()),
        };
        match up.authenticate_none("dev").await {
            Ok(r) if r.success() => Ok(up),
            _ => Err("the environment's SSH server did not let the gateway in".into()),
        }
    }

    /// Sends a request on a channel's guest half and waits for its answer.
    async fn ask<F, Fut>(&mut self, id: ChannelId, send: F) -> bool
    where
        F: FnOnce(Arc<russh::ChannelWriteHalf<russh::client::Msg>>) -> Fut,
        Fut: std::future::Future<Output = Result<(), russh::Error>>,
    {
        let Some(up) = self.channels.get(&id) else {
            return false;
        };
        let (tx, rx) = oneshot::channel();
        up.answers.lock().unwrap().push_back(tx);
        if send(up.write.clone()).await.is_err() {
            return false;
        }
        matches!(tokio::time::timeout(ANSWER_WITHIN, rx).await, Ok(Ok(true)))
    }

    /// Answers a session channel opened while the environment cannot be
    /// reached: its program says why, and exits 1.
    async fn refuse(&self, id: ChannelId, session: &mut Session) {
        let why = self.refused.clone().unwrap_or_default();
        let h = session.handle();
        let _ = session.channel_success(id);
        tokio::spawn(async move {
            let _ = h
                .extended_data(id, 1, bytes::Bytes::from(format!("hangar: {why}\r\n")))
                .await;
            let _ = h.exit_status_request(id, 1).await;
            let _ = h.close(id).await;
        });
    }
}

fn reply(session: &mut Session, id: ChannelId, ok: bool) -> Result<(), russh::Error> {
    if ok {
        session.channel_success(id)
    } else {
        session.channel_failure(id)
    }
}

/// Carries a channel's data both ways until it ends: the client's input
/// until it ends it, the guest's output, error stream, exit status and end
/// back in the order the guest sent them.
async fn relay(
    mut down: Channel<Msg>,
    handle: server::Handle,
    mut up: russh::ChannelReadHalf,
    answers: Arc<Mutex<VecDeque<oneshot::Sender<bool>>>>,
    write: Arc<russh::ChannelWriteHalf<russh::client::Msg>>,
) {
    let id = down.id();
    let mut client_open = true;
    loop {
        tokio::select! {
            m = down.wait(), if client_open => match m {
                Some(ChannelMsg::Data { data }) => {
                    if write.data_bytes(bytes::Bytes::copy_from_slice(&data)).await.is_err() {
                        break;
                    }
                }
                Some(ChannelMsg::Eof) => {
                    let _ = write.eof().await;
                }
                Some(ChannelMsg::Close) | None => {
                    client_open = false;
                    let _ = write.close().await;
                }
                Some(_) => {}
            },
            m = up.wait() => match m {
                Some(ChannelMsg::Data { data }) => {
                    if down.data(&data[..]).await.is_err() {
                        break;
                    }
                }
                Some(ChannelMsg::ExtendedData { data, ext }) => {
                    let _ = down.extended_data(ext, &data[..]).await;
                }
                Some(ChannelMsg::Eof) => {
                    let _ = down.eof().await;
                }
                Some(ChannelMsg::ExitStatus { exit_status }) => {
                    let _ = down.exit_status(exit_status).await;
                }
                Some(ChannelMsg::ExitSignal { signal_name, core_dumped, error_message, lang_tag }) => {
                    let _ = handle
                        .exit_signal_request(id, signal_name, core_dumped, error_message, lang_tag)
                        .await;
                }
                Some(ChannelMsg::Success) => {
                    if let Some(tx) = answers.lock().unwrap().pop_front() {
                        let _ = tx.send(true);
                    }
                }
                Some(ChannelMsg::Failure) => {
                    if let Some(tx) = answers.lock().unwrap().pop_front() {
                        let _ = tx.send(false);
                    }
                }
                Some(ChannelMsg::Close) | None => {
                    let _ = down.close().await;
                    break;
                }
                Some(_) => {}
            },
        }
    }
}

impl server::Handler for Conn {
    type Error = russh::Error;

    async fn auth_publickey_offered(
        &mut self,
        user: &str,
        offer: PublicKeyOffer<'_>,
    ) -> Result<Auth, Self::Error> {
        self.found = self.find(user, offer.public_key).await;
        Ok(match self.found {
            Some(_) => Auth::Accept,
            None => Auth::reject(),
        })
    }

    async fn auth_publickey(
        &mut self,
        user: &str,
        key: VerifiedPublicKey<'_>,
    ) -> Result<Auth, Self::Error> {
        let matches = self
            .found
            .as_ref()
            .is_some_and(|f| f.fingerprint == crate::keys::fingerprint(key.public_key));
        if !matches {
            self.found = self.find(user, key.public_key).await;
        }
        Ok(match self.found {
            Some(_) => Auth::Accept,
            None => Auth::reject(),
        })
    }

    async fn auth_succeeded(&mut self, session: &mut Session) -> Result<(), Self::Error> {
        self.client = String::from_utf8_lossy(session.remote_sshid()).into_owned();
        let Some(v) = self.view().await else {
            self.refused = Some("the environment is gone".into());
            return Ok(());
        };
        match self.dial(&v).await {
            Ok(up) => {
                self.up = Some(up);
                let fp = self
                    .found
                    .as_ref()
                    .map(|f| f.fingerprint.clone())
                    .unwrap_or_default();
                envs::record_access(
                    &self.app.store,
                    &self.actor(),
                    &v,
                    "environment.ssh_connect",
                    serde_json::json!({ "key": fp, "client": self.client }),
                )
                .await;
            }
            Err(why) => {
                envs::record_access(
                    &self.app.store,
                    &self.actor(),
                    &v,
                    "environment.ssh_refused",
                    serde_json::json!({ "reason": why }),
                )
                .await;
                self.refused = Some(why);
            }
        }
        Ok(())
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: ChannelOpenHandle,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let Some(up) = &self.up else {
            reply.accept().await;
            return Ok(());
        };
        match up.channel_open_session().await {
            Ok(uch) => {
                reply.accept().await;
                let (read, write) = uch.split();
                let write = Arc::new(write);
                let answers = Arc::new(Mutex::new(VecDeque::new()));
                self.channels.insert(
                    channel.id(),
                    Upstream {
                        write: write.clone(),
                        answers: answers.clone(),
                    },
                );
                tokio::spawn(relay(channel, session.handle(), read, answers, write));
            }
            Err(_) => reply.reject(ChannelOpenFailure::ConnectFailed).await,
        }
        Ok(())
    }

    async fn channel_open_direct_tcpip(
        &mut self,
        channel: Channel<Msg>,
        host: &str,
        port: u32,
        originator: &str,
        originator_port: u32,
        reply: ChannelOpenHandle,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let Some(up) = &self.up else {
            reply.reject(ChannelOpenFailure::ConnectFailed).await;
            return Ok(());
        };
        match up
            .channel_open_direct_tcpip(host, port, originator, originator_port)
            .await
        {
            Ok(uch) => {
                reply.accept().await;
                let (read, write) = uch.split();
                tokio::spawn(relay(
                    channel,
                    session.handle(),
                    read,
                    Arc::default(),
                    Arc::new(write),
                ));
            }
            Err(russh::Error::ChannelOpenFailure(why)) => reply.reject(why).await,
            Err(_) => reply.reject(ChannelOpenFailure::ConnectFailed).await,
        }
        Ok(())
    }

    async fn channel_close(
        &mut self,
        id: ChannelId,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.channels.remove(&id);
        Ok(())
    }

    async fn pty_request(
        &mut self,
        id: ChannelId,
        term: &str,
        cols: u32,
        rows: u32,
        pix_width: u32,
        pix_height: u32,
        modes: &[(Pty, u32)],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        if self.up.is_none() {
            return reply(session, id, true);
        }
        let (term, modes) = (term.to_string(), modes.to_vec());
        let ok = self
            .ask(id, |w| async move {
                w.request_pty(true, &term, cols, rows, pix_width, pix_height, &modes)
                    .await
            })
            .await;
        reply(session, id, ok)
    }

    async fn env_request(
        &mut self,
        id: ChannelId,
        name: &str,
        value: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        if self.up.is_none() {
            return reply(session, id, true);
        }
        let (name, value) = (name.to_string(), value.to_string());
        let ok = self
            .ask(id, |w| async move { w.set_env(true, name, value).await })
            .await;
        reply(session, id, ok)
    }

    async fn shell_request(
        &mut self,
        id: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        if self.up.is_none() {
            self.refuse(id, session).await;
            return Ok(());
        }
        let ok = self
            .ask(id, |w| async move { w.request_shell(true).await })
            .await;
        reply(session, id, ok)
    }

    async fn exec_request(
        &mut self,
        id: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        if self.up.is_none() {
            self.refuse(id, session).await;
            return Ok(());
        }
        let command = data.to_vec();
        let ok = self
            .ask(id, |w| async move { w.exec(true, command).await })
            .await;
        reply(session, id, ok)
    }

    async fn subsystem_request(
        &mut self,
        id: ChannelId,
        name: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        if self.up.is_none() {
            self.refuse(id, session).await;
            return Ok(());
        }
        let name = name.to_string();
        let ok = self
            .ask(id, |w| async move { w.request_subsystem(true, name).await })
            .await;
        reply(session, id, ok)
    }

    /// A window's new size: the guest is told, and nothing waits on its
    /// answer, since output may be flowing meanwhile.
    async fn window_change_request(
        &mut self,
        id: ChannelId,
        cols: u32,
        rows: u32,
        pix_width: u32,
        pix_height: u32,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        if let Some(up) = self.channels.get(&id) {
            let _ = up
                .write
                .window_change(cols, rows, pix_width, pix_height)
                .await;
        }
        reply(session, id, true)
    }

    async fn signal(
        &mut self,
        id: ChannelId,
        signal: Sig,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        if let Some(up) = self.channels.get(&id) {
            let _ = up.write.signal(signal).await;
        }
        reply(session, id, true)
    }
}

impl Drop for Conn {
    fn drop(&mut self) {
        if self.up.is_none() {
            return;
        }
        let (app, actor, found) = (self.app.clone(), self.actor(), self.found.clone());
        let seconds = self.started.elapsed().as_secs();
        let p = self.principal();
        tokio::spawn(async move {
            let (Some(found), Some(p)) = (found, p) else {
                return;
            };
            if let Ok(Some(v)) = envs::get(&app.store, &p, found.env).await {
                envs::record_access(
                    &app.store,
                    &actor,
                    &v,
                    "environment.ssh_disconnect",
                    serde_json::json!({ "seconds": seconds }),
                )
                .await;
            }
        });
    }
}
