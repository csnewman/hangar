//! The SSH gateway: signing in with a key from the profile, reaching an
//! environment by name, and running a command in it through the tunnel.

use std::sync::Arc;

use hangar_suite::{Server, body, eventually, frontendapi, machine};
use russh::keys::ssh_key::private::Ed25519Keypair;
use russh::keys::{PrivateKey, PrivateKeyWithHashAlg, PublicKeyOrCertificate};
use serde_json::json;

struct Trusting;

impl russh::client::Handler for Trusting {
    type Error = russh::Error;

    async fn check_server_key(&mut self, _: &PublicKeyOrCertificate) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

fn key(seed: u8) -> PrivateKey {
    PrivateKey::from(Ed25519Keypair::from_seed(&[seed; 32]))
}

async fn running(admin: &frontendapi::Client, name: &str) {
    let t = admin
        .create_template(&body(machine("Shell", "private")))
        .await
        .unwrap()
        .into_inner();
    let e = admin
        .create_environment(&body(json!({ "template_id": t.id, "name": name })))
        .await
        .unwrap()
        .into_inner();
    eventually("the environment running", || async {
        let e = admin.get_environment(&e.id).await.ok()?.into_inner();
        (serde_json::to_value(e.phase).ok()? == json!("running")).then_some(())
    })
    .await;
}

/// Signs in to the gateway as user with key and runs command, giving its
/// output and exit status; or None when the gateway refuses the key.
async fn run(s: &Server, user: &str, key: &PrivateKey, command: &str) -> Option<(String, u32)> {
    let config = Arc::new(russh::client::Config::default());
    let mut session = russh::client::connect(config, s.ssh_addr(), Trusting)
        .await
        .expect("reaching the gateway");
    let auth = session
        .authenticate_publickey(
            user,
            PrivateKeyWithHashAlg::new(Arc::new(key.clone()), None),
        )
        .await
        .expect("asking to sign in");
    if !auth.success() {
        return None;
    }
    let mut ch = session.channel_open_session().await.ok()?;
    ch.exec(true, command).await.expect("running it");
    let (mut out, mut status) = (Vec::new(), None);
    while let Some(m) = tokio::time::timeout(std::time::Duration::from_secs(10), ch.wait())
        .await
        .expect("an answer in time")
    {
        match m {
            russh::ChannelMsg::Data { data } => out.extend_from_slice(&data),
            russh::ChannelMsg::ExitStatus { exit_status } => status = Some(exit_status),
            _ => {}
        }
    }
    Some((String::from_utf8_lossy(&out).into_owned(), status?))
}

#[tokio::test]
async fn a_command_runs_over_the_gateway() {
    let Some(s) = Server::start() else { return };
    let admin = s.admin().await;
    let _w = s.worker("w");
    running(&admin, "shell").await;
    let mine = key(1);
    admin
        .add_login_key(&body(json!({
            "public_key": format!("{} mine", mine.public_key().to_openssh().unwrap()),
        })))
        .await
        .expect("adding a sign-in key");

    let out = run(&s, "shell", &mine, "echo hello").await;
    assert_eq!(out, Some(("ran echo hello\n".into(), 0)));

    let stranger = key(2);
    assert_eq!(
        run(&s, "shell", &stranger, "true").await,
        None,
        "a key never added signed in"
    );
}

#[tokio::test]
async fn someone_elses_environment_is_out_of_reach() {
    let Some(s) = Server::start() else { return };
    let admin = s.admin().await;
    let bob = s.user(&admin, "bob").await;
    let _w = s.worker("w");
    running(&admin, "shell").await;
    let bobs = key(3);
    bob.add_login_key(&body(json!({
        "public_key": format!("{} bobs", bobs.public_key().to_openssh().unwrap()),
    })))
    .await
    .unwrap();
    for user in ["shell", "admin/shell"] {
        let out = run(&s, user, &bobs, "true").await;
        assert!(
            !matches!(out, Some((_, 0))),
            "bob ran a command in admin's environment as {user}: {out:?}"
        );
    }
}

/// Remembers the host key the gateway shows.
struct Noting(Arc<std::sync::Mutex<Option<String>>>);

impl russh::client::Handler for Noting {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        if let PublicKeyOrCertificate::PublicKey { key, .. } = key {
            let mut k = key.clone();
            k.set_comment("");
            *self.0.lock().unwrap() = k.to_openssh().ok();
        }
        Ok(true)
    }
}

/// Signs in as user with key and runs command, giving its standard
/// output, its error stream and its exit status.
async fn run_all(
    s: &Server,
    user: &str,
    key: &PrivateKey,
    command: &str,
) -> (String, String, Option<u32>, Option<String>) {
    let shown = Arc::new(std::sync::Mutex::new(None));
    let config = Arc::new(russh::client::Config::default());
    let mut session = russh::client::connect(config, s.ssh_addr(), Noting(shown.clone()))
        .await
        .expect("reaching the gateway");
    let auth = session
        .authenticate_publickey(
            user,
            PrivateKeyWithHashAlg::new(Arc::new(key.clone()), None),
        )
        .await
        .expect("asking to sign in");
    assert!(auth.success(), "{user} refused");
    let mut ch = session.channel_open_session().await.expect("a session");
    ch.exec(true, command).await.expect("running it");
    let (mut out, mut err, mut status) = (Vec::new(), Vec::new(), None);
    while let Some(m) = tokio::time::timeout(std::time::Duration::from_secs(10), ch.wait())
        .await
        .expect("an answer in time")
    {
        match m {
            russh::ChannelMsg::Data { data } => out.extend_from_slice(&data),
            russh::ChannelMsg::ExtendedData { data, .. } => err.extend_from_slice(&data),
            russh::ChannelMsg::ExitStatus { exit_status } => status = Some(exit_status),
            _ => {}
        }
    }
    let shown = shown.lock().unwrap().clone();
    (
        String::from_utf8_lossy(&out).into_owned(),
        String::from_utf8_lossy(&err).into_owned(),
        status,
        shown,
    )
}

/// The gateway shows the host key the server says it has, takes an
/// environment's short ID as its name, says why a stopped one cannot be
/// reached, and records each connection with the key it was made with.
#[tokio::test]
async fn the_gateway_is_what_the_server_says() {
    let Some(s) = Server::start() else { return };
    let admin = s.admin().await;
    let _w = s.worker("w");
    running(&admin, "shell").await;
    let mine = key(4);
    admin
        .add_login_key(&body(json!({
            "public_key": format!("{} mine", mine.public_key().to_openssh().unwrap()),
        })))
        .await
        .unwrap();
    let me = admin.get_me().await.unwrap().into_inner();
    let gateway = me.ssh.expect("the SSH gateway in Me");
    assert_eq!(gateway.port, s.ssh_addr().port() as i64);

    let (out, _, status, shown) = run_all(&s, "shell", &mine, "echo hi").await;
    assert_eq!((out.as_str(), status), ("ran echo hi\n", Some(0)));
    assert_eq!(
        shown.as_deref(),
        Some(gateway.host_key.as_str()),
        "the host key shown"
    );

    let env = admin
        .list_environments()
        .await
        .unwrap()
        .into_inner()
        .into_iter()
        .find(|e| e.name == "shell")
        .unwrap();
    let (out, _, status, _) = run_all(&s, &env.short_id, &mine, "true").await;
    assert_eq!(
        (out.as_str(), status),
        ("ran true\n", Some(0)),
        "by short ID"
    );

    let subject = vec![format!("environment:{}", env.id)];
    let events = admin
        .list_audit(None, None, Some(&subject))
        .await
        .unwrap()
        .into_inner();
    let connect = events
        .iter()
        .find(|e| e.action == "environment.ssh_connect")
        .expect("the connection in the audit log");
    let fingerprint = mine
        .public_key()
        .fingerprint(russh::keys::ssh_key::HashAlg::Sha256)
        .to_string();
    assert_eq!(connect.details["key"], json!(fingerprint));
    assert_eq!(
        connect.details["via"],
        json!(format!("ssh key {fingerprint}"))
    );

    admin.stop_environment(&env.id).await.unwrap();
    eventually("the environment stopped", || async {
        let e = admin.get_environment(&env.id).await.ok()?.into_inner();
        (serde_json::to_value(e.phase).ok()? == json!("stopped")).then_some(())
    })
    .await;
    let (_, err, status, _) = run_all(&s, "shell", &mine, "true").await;
    assert_eq!(status, Some(1));
    assert!(err.contains("not running: start it first"), "told {err:?}");
}
