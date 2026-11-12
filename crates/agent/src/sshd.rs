//! The SSH server inside an environment, on vsock port 8107, which Hangar's
//! SSH gateway reaches it through: `ssh <environment>@<hangar> -p 2222`,
//! and an editor's remote SSH over the same.
//!
//! The gateway has already signed the person in with their key, and only
//! the host reaches the port (`vsock::Listener`), so the server asks nothing
//! of whoever connects: it serves shells, commands, SFTP and local port
//! forwarding as the environment's user, as sshd would for them.

use std::collections::HashMap;
use std::io::{self, Read};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::time::Duration;

use russh::keys::PrivateKey;
use russh::keys::ssh_key::private::Ed25519Keypair;
use russh::server::{self, Auth, ChannelOpenHandle, Msg, Session};
use russh::{Channel, ChannelId, ChannelMsg, ChannelOpenFailure, MethodKind, MethodSet, SshId};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::fdio::FdStream;
use crate::terminal::{open_pty, winsize};
use crate::user::{self, User};

pub const PORT: u32 = 8107;

/// OpenSSH's SFTP server, in the places images have it.
const OPENSSH_SFTP: &[&str] = &[
    "/usr/lib/openssh/sftp-server",
    "/usr/libexec/openssh/sftp-server",
    "/usr/lib/sftp-server",
];

/// How long a forward waits for what it is asked to reach.
const FORWARD_WAIT: Duration = Duration::from_secs(10);

/// Serves SSH as one user.
pub struct Server {
    config: Arc<server::Config>,
    user: String,
    sftp: Option<String>,
}

impl Server {
    /// Serves as the user named, or as whoever runs this when there is no
    /// such user. Its host key is made afresh: the only client, the gateway,
    /// reaches it over a channel that is already the host's own, and checks
    /// nothing of it.
    pub fn new(user: &str) -> io::Result<Self> {
        let mut seed = [0; 32];
        std::fs::File::open("/dev/urandom")?.read_exact(&mut seed)?;
        let config = server::Config {
            server_id: SshId::Standard("SSH-2.0-hangar-agent".into()),
            methods: MethodSet::from(&[MethodKind::None][..]),
            auth_rejection_time: Duration::ZERO,
            keys: vec![PrivateKey::from(Ed25519Keypair::from_seed(&seed))],
            // An idle shell is the person's to end.
            inactivity_timeout: None,
            nodelay: true,
            ..server::Config::default()
        };
        let sftp = std::env::current_exe()
            .ok()
            .map(|exe| format!("{} sftp-server", exe.display()));
        Ok(Self {
            config: Arc::new(config),
            user: user.to_string(),
            sftp,
        })
    }

    /// Has the sftp subsystem run `command`, through the user's shell,
    /// where the image has no OpenSSH sftp-server; none refuses it there.
    pub fn sftp_command(mut self, command: Option<String>) -> Self {
        self.sftp = command;
        self
    }

    /// Holds one SSH connection until it closes.
    pub async fn serve<S>(&self, conn: S)
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let handler = Conn {
            user: self.user.clone(),
            sftp: self.sftp.clone(),
            channels: HashMap::new(),
        };
        match server::run_stream(self.config.clone(), conn, handler).await {
            Ok(running) => {
                let _ = running.await;
            }
            Err(e) => eprintln!("hangar-agent: ssh: {e}"),
        }
    }
}

/// One connection.
struct Conn {
    user: String,
    sftp: Option<String>,
    channels: HashMap<ChannelId, Chan>,
}

/// A session channel: what it asked for, until it starts a program.
#[derive(Default)]
struct Chan {
    /// The channel, until a program is started on it.
    channel: Option<Channel<Msg>>,
    env: Vec<(String, String)>,
    pty: Option<PtyRequest>,
    /// The terminal's master, once a program runs on one.
    master: Option<Arc<FdStream>>,
}

struct PtyRequest {
    term: String,
    cols: u16,
    rows: u16,
}

fn reply(session: &mut Session, id: ChannelId, ok: bool) -> Result<(), russh::Error> {
    if ok {
        session.channel_success(id)
    } else {
        session.channel_failure(id)
    }
}

/// A terminal size from the client's, a missing one being 80 by 24.
fn size(cols: u32, rows: u32) -> (u16, u16) {
    let clamp = |n: u32, otherwise| match u16::try_from(n) {
        Ok(0) | Err(_) => otherwise,
        Ok(n) => n,
    };
    (clamp(cols, 80), clamp(rows, 24))
}

impl server::Handler for Conn {
    type Error = russh::Error;

    async fn auth_none(&mut self, _user: &str) -> Result<Auth, Self::Error> {
        Ok(Auth::Accept)
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.channels.insert(
            channel.id(),
            Chan {
                channel: Some(channel),
                ..Chan::default()
            },
        );
        reply.accept().await;
        Ok(())
    }

    /// A local port forward (`ssh -L`), to where it asks, from inside the
    /// environment.
    async fn channel_open_direct_tcpip(
        &mut self,
        channel: Channel<Msg>,
        host: &str,
        port: u32,
        _originator: &str,
        _originator_port: u32,
        reply: ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        let host = host.to_string();
        tokio::spawn(async move {
            let target = async {
                let port = u16::try_from(port).map_err(|_| io::ErrorKind::InvalidInput)?;
                tokio::time::timeout(FORWARD_WAIT, TcpStream::connect((host.as_str(), port)))
                    .await
                    .map_err(|_| io::ErrorKind::TimedOut)?
            };
            match target.await {
                Ok(mut tcp) => {
                    reply.accept().await;
                    let mut ch = channel.into_stream();
                    let _ = tokio::io::copy_bidirectional(&mut ch, &mut tcp).await;
                }
                Err(_) => reply.reject(ChannelOpenFailure::ConnectFailed).await,
            }
        });
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

    async fn env_request(
        &mut self,
        id: ChannelId,
        name: &str,
        value: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let ok = match self.channels.get_mut(&id) {
            Some(c) if c.channel.is_some() => {
                c.env.push((name.to_string(), value.to_string()));
                true
            }
            _ => false,
        };
        reply(session, id, ok)
    }

    async fn pty_request(
        &mut self,
        id: ChannelId,
        term: &str,
        cols: u32,
        rows: u32,
        _pix_width: u32,
        _pix_height: u32,
        _modes: &[(russh::Pty, u32)],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let ok = match self.channels.get_mut(&id) {
            Some(c) if c.channel.is_some() => {
                let (cols, rows) = size(cols, rows);
                c.pty = Some(PtyRequest {
                    term: term.to_string(),
                    cols,
                    rows,
                });
                true
            }
            _ => false,
        };
        reply(session, id, ok)
    }

    async fn window_change_request(
        &mut self,
        id: ChannelId,
        cols: u32,
        rows: u32,
        _pix_width: u32,
        _pix_height: u32,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let master = self.channels.get(&id).and_then(|c| c.master.clone());
        if let Some(m) = &master {
            let (cols, rows) = size(cols, rows);
            let ws = winsize(cols, rows);
            // SAFETY: TIOCSWINSZ with a winsize, on a terminal this channel
            // owns.
            unsafe { libc::ioctl(m.raw(), libc::TIOCSWINSZ as _, &ws as *const libc::winsize) };
        }
        reply(session, id, master.is_some())
    }

    async fn shell_request(
        &mut self,
        id: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.start(id, None, session)
    }

    async fn exec_request(
        &mut self,
        id: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let command = String::from_utf8_lossy(data).into_owned();
        self.start(id, Some(command), session)
    }

    async fn subsystem_request(
        &mut self,
        id: ChannelId,
        name: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let command = (name == "sftp")
            .then(|| {
                OPENSSH_SFTP
                    .iter()
                    .find(|p| Path::new(p).is_file())
                    .map(|p| p.to_string())
                    .or_else(|| self.sftp.clone())
            })
            .flatten();
        match command {
            Some(c) => self.start(id, Some(c), session),
            None => reply(session, id, false),
        }
    }

    async fn signal(
        &mut self,
        id: ChannelId,
        _signal: russh::Sig,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        reply(session, id, true)
    }
}

impl Conn {
    /// Starts a program on a channel: the user's login shell, or a command
    /// given to their shell, as sshd would; on a terminal if one was asked
    /// for.
    fn start(
        &mut self,
        id: ChannelId,
        command: Option<String>,
        session: &mut Session,
    ) -> Result<(), russh::Error> {
        let Some(c) = self.channels.get_mut(&id) else {
            return reply(session, id, false);
        };
        let Some(channel) = c.channel.take() else {
            return reply(session, id, false);
        };
        let cmd = command_for(&self.user, command.as_deref(), &c.env, c.pty.as_ref());
        let started = match &c.pty {
            Some(p) => spawn_pty(cmd, p).map(|(child, master)| (child, Some(master))),
            None => spawn_piped(cmd).map(|child| (child, None)),
        };
        match started {
            Ok((child, Some(master))) => {
                c.master = Some(master.clone());
                tokio::spawn(run_pty(channel, child, master));
                reply(session, id, true)
            }
            Ok((child, None)) => {
                tokio::spawn(run_piped(channel, child));
                reply(session, id, true)
            }
            // The channel may ask again.
            Err(e) => {
                c.channel = Some(channel);
                session.extended_data(id, 1, format!("{e}\r\n").into_bytes())?;
                reply(session, id, false)
            }
        }
    }
}

/// What a request runs, as the user, with the environment sshd would give
/// it and what the client sent.
fn command_for(
    name: &str,
    command: Option<&str>,
    env: &[(String, String)],
    pty: Option<&PtyRequest>,
) -> Command {
    let u = User::lookup(name).unwrap_or_else(User::current);
    let shell = u.login_shell();
    let mut cmd = Command::new(&shell);
    match command {
        // argv[0] starting with "-" makes it a login shell.
        None => {
            let base = Path::new(&shell).file_name().unwrap_or_default();
            cmd.arg0(format!("-{}", base.to_string_lossy()));
        }
        Some(c) => {
            cmd.arg("-c").arg(c);
        }
    }
    cmd.current_dir(&u.home)
        .env_clear()
        .env("HOME", &u.home)
        .env("USER", &u.name)
        .env("LOGNAME", &u.name)
        .env("SHELL", &shell)
        .env("SSH_AUTH_SOCK", user::SSH_AUTH_SOCK)
        .env("PATH", user::PATH)
        .env("LANG", "C.UTF-8")
        .env("SSH_CONNECTION", "127.0.0.1 0 127.0.0.1 22")
        .envs(user::session_env())
        .envs(env.iter().map(|(k, v)| (k, v)));
    if let Some(p) = pty {
        cmd.env("TERM", &p.term);
    }
    user::prepare(&mut cmd, Some(&u), user::oom::USER);
    cmd
}

fn spawn_piped(cmd: Command) -> Result<tokio::process::Child, io::Error> {
    let mut cmd = tokio::process::Command::from(cmd);
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
}

fn spawn_pty(
    mut cmd: Command,
    p: &PtyRequest,
) -> Result<(tokio::process::Child, Arc<FdStream>), io::Error> {
    let (master, slave) = open_pty(p.cols, p.rows)?;
    cmd.stdin(Stdio::from(slave.try_clone()?))
        .stdout(Stdio::from(slave.try_clone()?))
        .stderr(Stdio::from(slave));
    // SAFETY: setsid and ioctl are async-signal-safe.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut cmd = tokio::process::Command::from(cmd);
    let child = cmd.spawn()?;
    // Its copies of the terminal's other side go, so the terminal reads as
    // ended once the program and what it started have closed theirs.
    drop(cmd);
    Ok((child, Arc::new(FdStream::new(master)?)))
}

/// Carries a program on pipes: the channel's data to its input, its output
/// and errors back, then its status.
async fn run_piped(channel: Channel<Msg>, mut child: tokio::process::Child) {
    let (mut rd, wr) = channel.split();
    let mut stdin = child.stdin.take();
    let input = crate::serve::AbortOnDrop(tokio::spawn(async move {
        while let Some(m) = rd.wait().await {
            match m {
                ChannelMsg::Data { data } => {
                    if let Some(s) = stdin.as_mut()
                        && s.write_all(&data).await.is_err()
                    {
                        stdin = None;
                    }
                }
                // Closing its input is how the program hears the end.
                ChannelMsg::Eof => stdin = None,
                _ => {}
            }
        }
    }));
    tokio::join!(
        copy_out(child.stdout.take(), wr.make_writer()),
        copy_out(child.stderr.take(), wr.make_writer_ext(Some(1))),
    );
    let status = child.wait().await;
    finish(&wr, status).await;
    drop(input);
}

/// Copies a program's output to the channel, until the program and what it
/// started have closed it.
async fn copy_out(from: Option<impl AsyncRead + Unpin>, mut to: impl AsyncWrite + Unpin) {
    if let Some(mut from) = from {
        let _ = tokio::io::copy(&mut from, &mut to).await;
    }
}

/// Carries a program on a terminal, until it and what it started have
/// closed the terminal. A channel that closes first hangs the terminal up,
/// as sshd does.
async fn run_pty(channel: Channel<Msg>, mut child: tokio::process::Child, master: Arc<FdStream>) {
    let (mut rd, wr) = channel.split();
    let pid = child.id().map(|p| p as libc::pid_t);
    let input = {
        let master = master.clone();
        crate::serve::AbortOnDrop(tokio::spawn(async move {
            while let Some(m) = rd.wait().await {
                if let ChannelMsg::Data { data } = m
                    && master.write_all(&data).await.is_err()
                {
                    break;
                }
            }
            if let Some(pid) = pid {
                // SAFETY: kill(2) of the program's own session, which it
                // leads.
                unsafe { libc::kill(-pid, libc::SIGHUP) };
            }
        }))
    };
    let mut out = wr.make_writer();
    let mut buf = vec![0; 32 << 10];
    // A terminal whose other side has closed reads as an error, EIO.
    while let Ok(n @ 1..) = master.read(&mut buf).await {
        if out.write_all(&buf[..n]).await.is_err() {
            break;
        }
    }
    let status = child.wait().await;
    finish(&wr, status).await;
    drop(input);
}

/// Ends a channel as sshd does: the end of the output, then the program's
/// status, then the channel.
async fn finish(wr: &russh::ChannelWriteHalf<Msg>, status: io::Result<ExitStatus>) {
    let code = match status {
        Ok(s) => match (s.code(), s.signal()) {
            (Some(c), _) => c as u32,
            (None, Some(sig)) => 128 + sig as u32,
            (None, None) => 255,
        },
        Err(_) => 255,
    };
    let _ = wr.eof().await;
    let _ = wr.exit_status(code).await;
    let _ = wr.close().await;
}
