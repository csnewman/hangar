//! The generated APIs as a client sees them: what reads, and the operations
//! that answer two ways.

use hangar_openapi::{SignInStep, clientapi, frontendapi};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

fn client(url: &str) -> clientapi::Client {
    clientapi::Client::new_with_client(url, hangar_openapi::http_client().unwrap().build().unwrap())
}

fn frontend(url: &str) -> frontendapi::Client {
    frontendapi::Client::new_with_client(
        url,
        hangar_openapi::http_client().unwrap().build().unwrap(),
    )
}

/// Answers one request with status and body, and gives the address to
/// send it to.
async fn answer_once(status: &'static str, body: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut conn, _) = listener.accept().await.unwrap();
        let mut request = vec![0u8; 8192];
        let _ = conn.read(&mut request).await;
        let response = format!(
            "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        conn.write_all(response.as_bytes()).await.unwrap();
    });
    format!("http://{addr}")
}

/// The client API's enums take a value a newer server sends, as its
/// compatibility rule asks.
#[test]
fn client_enums_take_new_values() {
    let s: clientapi::types::EnvironmentSettings =
        serde_json::from_str(r#"{"cpus":2,"memory_mib":2048,"display":"hologram","gpu":"none"}"#)
            .unwrap();
    assert_eq!(s.display, "hologram");
}

/// The web UI's API ships with its UI, and refuses a value it does not
/// know.
#[test]
fn frontend_enums_are_strict() {
    let r = serde_json::from_str::<frontendapi::types::SignalProcess>(r#"{"signal":"HUP"}"#);
    assert!(r.is_err(), "{r:?}");
}

#[tokio::test]
async fn sign_in_token() {
    let url = answer_once("200 OK", r#"{"name":"laptop","token":"hgr_x"}"#).await;
    let c = client(&url);
    let step = c
        .finish_sign_in_step(&clientapi::types::FinishSignIn {
            device_code: "d".into(),
        })
        .await
        .unwrap();
    assert_eq!(
        step,
        SignInStep::Token(clientapi::types::SignInToken {
            name: "laptop".into(),
            token: "hgr_x".into()
        })
    );
}

#[tokio::test]
async fn sign_in_waiting() {
    let url = answer_once(
        "202 Accepted",
        r#"{"error":"not yet approved","code":"sign_in_pending"}"#,
    )
    .await;
    let c = client(&url);
    let step = c
        .finish_sign_in_step(&clientapi::types::FinishSignIn {
            device_code: "d".into(),
        })
        .await
        .unwrap();
    assert_eq!(
        step,
        SignInStep::Waiting(clientapi::types::Error {
            error: "not yet approved".into(),
            code: Some("sign_in_pending".into()),
        })
    );
}

#[tokio::test]
async fn delete_gone_at_once() {
    let url = answer_once("204 No Content", "").await;
    let c = frontend(&url);
    assert_eq!(c.delete_environment_or_gone("e1").await.unwrap(), None);
}

#[tokio::test]
async fn delete_refused() {
    let url = answer_once("404 Not Found", r#"{"error":"no such environment"}"#).await;
    let c = client(&url);
    let e = c.delete_environment_or_gone("e1").await.unwrap_err();
    assert_eq!(e.status().map(|s| s.as_u16()), Some(404));
}
