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
