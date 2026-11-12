//! The SSH server, through russh's client as the gateway would reach it:
//! commands on pipes and on a terminal, the client's input and environment,
//! SFTP through `hangar-agent-rs sftp-server`, and port forwarding.
//!
//! There is no user named as the server's, so it serves as whoever runs the
//! tests.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use hangar_agent::sshd::Server;
use russh::client::{self, Msg};
use russh::{Channel, ChannelMsg};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::timeout;

const WAIT: Duration = Duration::from_secs(20);

struct Client;

impl client::Handler for Client {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        _key: &russh::keys::PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

fn server() -> Arc<Server> {
    let sftp = format!("{} sftp-server", env!("CARGO_BIN_EXE_hangar-agent-rs"));
    Arc::new(
        Server::new("no-such-user-here")
            .unwrap()
            .sftp_command(Some(sftp)),
    )
}

/// A connection, signed in as the gateway signs in: asking nothing.
async fn connect() -> client::Handle<Client> {
    let (a, b) = tokio::io::duplex(1 << 20);
    let s = server();
    tokio::spawn(async move { s.serve(b).await });
    let config = Arc::new(client::Config::default());
    let mut h = client::connect_stream(config, a, Client).await.unwrap();
    assert!(h.authenticate_none("dev").await.unwrap().success());
    h
}

/// What a channel's program wrote, to its output and its errors, and how
/// it exited.
async fn collect(ch: &mut Channel<Msg>) -> (String, String, Option<u32>) {
    let (mut out, mut err, mut code) = (vec![], vec![], None);
    timeout(WAIT, async {
        while let Some(m) = ch.wait().await {
            match m {
                ChannelMsg::Data { data } => out.extend_from_slice(&data),
                ChannelMsg::ExtendedData { data, ext: 1 } => err.extend_from_slice(&data),
                ChannelMsg::ExitStatus { exit_status } => code = Some(exit_status),
                _ => {}
            }
        }
    })
    .await
    .expect("the program ended");
    (
        String::from_utf8_lossy(&out).into_owned(),
        String::from_utf8_lossy(&err).into_owned(),
        code,
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn commands_on_pipes() {
    let h = connect().await;
    let mut ch = h.channel_open_session().await.unwrap();
    ch.set_env(true, "HANGAR_TEST", "from the client")
        .await
        .unwrap();
    ch.exec(
        true,
        "echo out $HANGAR_TEST; echo err >&2; test -t 0 || echo no terminal; exit 3",
    )
    .await
    .unwrap();
    let (out, err, code) = collect(&mut ch).await;
    assert_eq!(out, "out from the client\nno terminal\n");
    assert_eq!(err, "err\n");
    assert_eq!(code, Some(3));

    // Input, to its end.
    let mut ch = h.channel_open_session().await.unwrap();
    ch.exec(true, "tr a-z A-Z").await.unwrap();
    ch.data(&b"hello\n"[..]).await.unwrap();
    ch.eof().await.unwrap();
    let (out, _, code) = collect(&mut ch).await;
    assert_eq!((out.as_str(), code), ("HELLO\n", Some(0)));

    // Killed by a signal.
    let mut ch = h.channel_open_session().await.unwrap();
    ch.exec(true, "kill -TERM $$").await.unwrap();
    assert_eq!(collect(&mut ch).await.2, Some(128 + 15));
}

#[tokio::test(flavor = "multi_thread")]
async fn commands_on_a_terminal() {
    let h = connect().await;
    let mut ch = h.channel_open_session().await.unwrap();
    ch.request_pty(true, "xterm-256color", 100, 30, 0, 0, &[])
        .await
        .unwrap();
    ch.exec(
        true,
        "test -t 0 && echo terminal $TERM; stty size; read l; stty size",
    )
    .await
    .unwrap();
    // A window change before the line it waits for.
    tokio::time::sleep(Duration::from_millis(300)).await;
    ch.window_change(120, 40, 0, 0).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    ch.data(&b"go\n"[..]).await.unwrap();
    let (out, _, code) = collect(&mut ch).await;
    assert!(out.contains("terminal xterm-256color"), "{out:?}");
    assert!(out.contains("30 100"), "{out:?}");
    assert!(out.contains("40 120"), "{out:?}");
    assert_eq!(code, Some(0));
}

#[tokio::test(flavor = "multi_thread")]
async fn sftp() {
    let dir = PathBuf::from(format!("/tmp/hs-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let d = std::fs::canonicalize(&dir).unwrap().display().to_string();

    let h = connect().await;
    let ch = h.channel_open_session().await.unwrap();
    ch.request_subsystem(true, "sftp").await.unwrap();
    let s = timeout(WAIT, russh_sftp::client::SftpSession::new(ch.into_stream()))
        .await
        .expect("sftp started")
        .unwrap();
    let mut f = s.create(format!("{d}/a.txt")).await.unwrap();
    f.write_all(b"hello sftp").await.unwrap();
    f.shutdown().await.unwrap();
    assert_eq!(s.read(format!("{d}/a.txt")).await.unwrap(), b"hello sftp");
    s.create_dir(format!("{d}/sub")).await.unwrap();
    s.rename(format!("{d}/a.txt"), format!("{d}/sub/b.txt"))
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.join("sub/b.txt")).unwrap(),
        "hello sftp"
    );
    let md = s.metadata(format!("{d}/sub/b.txt")).await.unwrap();
    assert_eq!(md.size, Some(10));
    // The target first, as OpenSSH's clients send it.
    s.symlink("sub/b.txt", format!("{d}/link")).await.unwrap();
    assert_eq!(
        std::fs::read_link(dir.join("link")).unwrap(),
        PathBuf::from("sub/b.txt")
    );
    assert_eq!(s.read_link(format!("{d}/link")).await.unwrap(), "sub/b.txt");
    assert!(
        s.symlink_metadata(format!("{d}/link"))
            .await
            .unwrap()
            .is_symlink()
    );
    let names: Vec<String> = s
        .read_dir(format!("{d}/sub"))
        .await
        .unwrap()
        .map(|e| e.file_name())
        .collect();
    assert_eq!(names, ["b.txt"]);
    assert_eq!(
        s.canonicalize(format!("{d}/sub/../sub/./b.txt"))
            .await
            .unwrap(),
        format!("{d}/sub/b.txt")
    );
    s.remove_file(format!("{d}/sub/b.txt")).await.unwrap();
    assert!(s.read(format!("{d}/sub/b.txt")).await.is_err());
    std::fs::remove_dir_all(&dir).unwrap();

    // Only sftp is a subsystem.
    let mut ch = h.channel_open_session().await.unwrap();
    ch.request_subsystem(true, "netconf").await.unwrap();
    let answer = timeout(WAIT, async {
        loop {
            match ch.wait().await {
                Some(ChannelMsg::Success) => return true,
                Some(ChannelMsg::Failure) => return false,
                Some(_) => {}
                None => panic!("the channel closed"),
            }
        }
    })
    .await
    .unwrap();
    assert!(!answer, "another subsystem was served");
}

#[tokio::test(flavor = "multi_thread")]
async fn local_port_forwarding() {
    let ln = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = ln.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (mut c, _) = ln.accept().await.unwrap();
        let mut b = [0; 4];
        c.read_exact(&mut b).await.unwrap();
        c.write_all(b"pong").await.unwrap();
    });
    let h = connect().await;
    let ch = h
        .channel_open_direct_tcpip("127.0.0.1", port as u32, "127.0.0.1", 0)
        .await
        .unwrap();
    let mut s = ch.into_stream();
    s.write_all(b"ping").await.unwrap();
    let mut b = [0; 4];
    timeout(WAIT, s.read_exact(&mut b)).await.unwrap().unwrap();
    assert_eq!(&b, b"pong");

    // Somewhere nothing listens.
    assert!(
        h.channel_open_direct_tcpip("127.0.0.1", 1, "127.0.0.1", 0)
            .await
            .is_err()
    );
}
