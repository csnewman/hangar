//! Terminals, through the client API's WebSocket, the server, the tunnel
//! and the worker to the guest's terminal service: attached, typed into,
//! listed, closed, and refused for an environment not running.

use futures_util::{SinkExt, StreamExt};
use hangar_suite::{Server, body, eventually, frontendapi, machine};
use serde_json::json;
use tokio_tungstenite::tungstenite::Message;

async fn running(admin: &frontendapi::Client) -> frontendapi::types::Environment {
    let t = admin
        .create_template(&body(machine("Shell", "private")))
        .await
        .unwrap()
        .into_inner();
    let e = admin
        .create_environment(&body(json!({ "template_id": t.id, "name": "shell" })))
        .await
        .unwrap()
        .into_inner();
    phase(admin, &e.id, "running").await;
    e
}

async fn phase(admin: &frontendapi::Client, id: &str, want: &str) {
    eventually(&format!("{id} {want}"), || async {
        let e = admin.get_environment(id).await.ok()?.into_inner();
        (serde_json::to_value(e.phase).ok()? == json!(want)).then_some(())
    })
    .await
}

/// The next binary message, a frame: its type and payload.
async fn frame<S>(ws: &mut S) -> (u8, Vec<u8>)
where
    S: StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    loop {
        let m = tokio::time::timeout(std::time::Duration::from_secs(10), ws.next())
            .await
            .expect("a frame in time")
            .expect("the socket open")
            .expect("a message");
        if let Message::Binary(b) = m {
            return (b[0], b[1..].to_vec());
        }
    }
}

#[tokio::test]
async fn a_terminal_is_attached_typed_into_and_closed() {
    let Some(s) = Server::start() else { return };
    let admin = s.admin().await;
    let _w = s.worker("w");
    let e = running(&admin).await;
    let token = s.token(&admin, "terminals").await;
    let c = s.client(&token);

    let mut ws = s
        .socket(
            &token,
            &format!("/environments/{}/terminal?cols=100&rows=30", e.id),
        )
        .await
        .expect("attaching");
    let Some(Ok(Message::Text(attached))) = ws.next().await else {
        panic!("no reply to the attach");
    };
    let attached: serde_json::Value = serde_json::from_str(&attached).unwrap();
    let session = attached["session"]["id"]
        .as_str()
        .expect("a session")
        .to_string();
    assert_eq!(attached["owner"], json!(true), "{attached}");
    let (kind, size) = frame(&mut ws).await;
    assert_eq!(kind, b's');
    assert_eq!(&size[..4], &[0, 100, 0, 30], "the size asked for");

    ws.send(Message::Binary(
        [b"i".as_slice(), b"echo hi\r"].concat().into(),
    ))
    .await
    .unwrap();
    assert_eq!(frame(&mut ws).await, (b'o', b"echo hi\r".to_vec()));

    let listed = c
        .list_terminals(&e.id)
        .await
        .expect("the terminals")
        .into_inner()
        .items;
    assert!(listed.iter().any(|t| t.id == session), "{listed:?}");
    drop(ws);
    c.close_terminal(&e.id, &session).await.expect("closing it");
    let listed = c.list_terminals(&e.id).await.unwrap().into_inner().items;
    assert!(listed.iter().all(|t| t.id != session), "{listed:?}");
}

#[tokio::test]
async fn a_shell_that_exits_ends_its_terminal() {
    let Some(s) = Server::start() else { return };
    let admin = s.admin().await;
    let _w = s.worker("w");
    let e = running(&admin).await;
    let token = s.token(&admin, "terminals").await;
    let mut ws = s
        .socket(&token, &format!("/environments/{}/terminal", e.id))
        .await
        .expect("attaching");
    let _ = ws.next().await;
    let _ = frame(&mut ws).await;
    ws.send(Message::Binary(
        [b"i".as_slice(), b"exit\r"].concat().into(),
    ))
    .await
    .unwrap();
    assert_eq!(frame(&mut ws).await.0, b'o');
    let (kind, status) = frame(&mut ws).await;
    assert_eq!((kind, status), (b'x', vec![0, 0, 0, 0]));
}

#[tokio::test]
async fn a_stopped_environment_has_no_terminal() {
    let Some(s) = Server::start() else { return };
    let admin = s.admin().await;
    let _w = s.worker("w");
    let e = running(&admin).await;
    admin.stop_environment(&e.id).await.unwrap();
    phase(&admin, &e.id, "stopped").await;
    let token = s.token(&admin, "terminals").await;
    let refused = s
        .socket(&token, &format!("/environments/{}/terminal", e.id))
        .await
        .err();
    assert_eq!(refused, Some(409));
    let r = s
        .http()
        .get(format!(
            "{}/api/v1/environments/{}/processes",
            s.url(),
            e.id
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 409);
    let body: serde_json::Value = r.json().await.unwrap();
    assert_eq!(body["code"], "environment_not_running", "{body}");
}
