//! What reaches into a running environment through its worker's tunnel:
//! its Code service, its desktop, its processes and its logs; and the
//! tunnel itself, kept up while nothing crosses it.

use futures_util::{SinkExt, StreamExt};
use hangar_suite::{Server, body, eventually, frontendapi, machine, status};
use serde_json::json;
use tokio_tungstenite::tungstenite::Message;

/// A running environment of a template body t, and a client API token for
/// its owner, the administrator.
async fn running(s: &Server, t: serde_json::Value) -> (frontendapi::Client, String, String) {
    let admin = s.admin().await;
    let t = admin.create_template(&body(t)).await.unwrap().into_inner();
    let e = admin
        .create_environment(&body(json!({ "template_id": t.id, "name": "streams" })))
        .await
        .unwrap()
        .into_inner();
    let id = e.id.clone();
    eventually("the environment running", || async {
        let e = admin.get_environment(&id).await.ok()?.into_inner();
        (serde_json::to_value(e.phase).ok()? == json!("running")).then_some(())
    })
    .await;
    let token = s.token(&admin, "streams").await;
    (admin, e.id, token)
}

/// The next binary or text message, passing over the socket's own pings.
async fn next<S>(ws: &mut S) -> Message
where
    S: futures_util::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    loop {
        let m = tokio::time::timeout(std::time::Duration::from_secs(20), ws.next())
            .await
            .expect("a message within 20 seconds")
            .expect("the socket open")
            .expect("a message");
        if matches!(m, Message::Binary(_) | Message::Text(_) | Message::Close(_)) {
            return m;
        }
    }
}

fn client_status<T, E>(r: Result<T, hangar_suite::clientapi::Error<E>>) -> Option<u16> {
    r.err().and_then(|e| e.status()).map(|s| s.as_u16())
}

#[tokio::test]
async fn the_code_service_is_reached_through_a_socket() {
    let Some(s) = Server::start() else { return };
    let _w = s.worker("w");
    let (_, id, token) = running(&s, machine("Code", "private")).await;

    let mut ws = s
        .socket(&token, &format!("/environments/{id}/code?root=/src/app"))
        .await
        .expect("opening the Code socket");
    match next(&mut ws).await {
        Message::Binary(b) => {
            let req: serde_json::Value = serde_json::from_slice(&b).unwrap();
            assert_eq!(req["root"], json!("/src/app"));
        }
        m => panic!("the service's first frame, not {m:?}"),
    }
    ws.send(Message::Binary(b"a frame".to_vec().into()))
        .await
        .unwrap();
    assert_eq!(
        next(&mut ws).await,
        Message::Binary(b"a frame".to_vec().into())
    );
    ws.send(Message::Text("ping".into())).await.unwrap();
    assert_eq!(next(&mut ws).await, Message::Text("pong".into()));
}

#[tokio::test]
async fn the_desktop_is_reached_and_resized() {
    let Some(s) = Server::start() else { return };
    let _w = s.worker("w");
    let mut t = machine("Desktop", "private");
    t["spec"]["display"] = json!("desktop");
    let (_, id, token) = running(&s, t).await;
    let c = s.client(&token);

    let mut ws = s
        .socket(&token, &format!("/environments/{id}/desktop"))
        .await
        .expect("opening the desktop");
    let mut got = Vec::new();
    while !got.ends_with(b"RFB 003.008\n") {
        match next(&mut ws).await {
            Message::Binary(b) => got.extend_from_slice(&b),
            m => panic!("RFB, not {m:?}"),
        }
    }
    ws.send(Message::Binary(b"from the viewer".to_vec().into()))
        .await
        .unwrap();
    let mut echoed = Vec::new();
    while echoed.len() < b"from the viewer".len() {
        if let Message::Binary(b) = next(&mut ws).await {
            echoed.extend_from_slice(&b);
        }
    }
    assert_eq!(echoed, b"from the viewer");

    c.resize_desktop(&id, &body(json!({ "width": 1280, "height": 800 })))
        .await
        .expect("resizing the desktop");
    assert_eq!(
        client_status(
            c.resize_desktop(&id, &body(json!({ "width": 320, "height": 200 })))
                .await
        ),
        Some(409)
    );
}

#[tokio::test]
async fn a_headless_environment_has_no_desktop() {
    let Some(s) = Server::start() else { return };
    let _w = s.worker("w");
    let (_, id, token) = running(&s, machine("Headless", "private")).await;
    assert_eq!(
        s.socket(&token, &format!("/environments/{id}/desktop"))
            .await
            .err(),
        Some(409)
    );
    let c = s.client(&token);
    assert_eq!(
        client_status(
            c.resize_desktop(&id, &body(json!({ "width": 1280, "height": 800 })))
                .await
        ),
        Some(409)
    );
}

#[tokio::test]
async fn processes_are_listed_and_signalled() {
    let Some(s) = Server::start() else { return };
    let _w = s.worker("w");
    let (admin, id, token) = running(&s, machine("Processes", "private")).await;
    let c = s.client(&token);

    let list = c
        .list_processes(&id)
        .await
        .expect("the process list")
        .into_inner();
    assert_eq!(list.cpus, 2);
    assert_eq!(list.processes.len(), 2);
    let init = list.processes.iter().find(|p| p.pid == 1).expect("init");
    assert_eq!(init.protected, Some(true));
    let web = admin
        .list_processes(&id)
        .await
        .expect("the web UI's list")
        .into_inner();
    assert_eq!(web.processes.len(), 2);

    c.signal_process(&id, 42, &body(json!({ "signal": "TERM" })))
        .await
        .expect("stopping a process");
    assert_eq!(
        client_status(
            c.signal_process(&id, 1, &body(json!({ "signal": "KILL" })))
                .await
        ),
        Some(409),
        "the client API signalled a protected process"
    );
    assert_eq!(
        status(
            admin
                .signal_process(&id, 1, &body(json!({ "signal": "KILL" })))
                .await
        ),
        Some(400),
        "the web UI's API signalled a protected process"
    );
    let subject = vec![format!("environment:{id}")];
    let events = admin
        .list_audit(None, None, Some(&subject))
        .await
        .unwrap()
        .into_inner();
    assert!(
        events
            .iter()
            .any(|e| e.action == "environment.signal_process" && e.details["pid"] == json!(42)),
        "the signal is not in the audit log"
    );
}

#[tokio::test]
async fn logs_are_read_from_the_worker() {
    let Some(s) = Server::start() else { return };
    let _w = s.worker("w");
    let (admin, id, token) = running(&s, machine("Logs", "private")).await;
    let c = s.client(&token);

    let log = |name: &str| serde_json::from_value(json!(name)).unwrap();
    let part = c
        .get_environment_log(&id, &log("worker"), None)
        .await
        .expect("the environment's worker log")
        .into_inner();
    assert_eq!(part.next, part.start + part.text.len() as i64);
    assert!(part.size >= part.next);
    let console = admin
        .get_environment_log(
            &id,
            serde_json::from_value(json!("console")).unwrap(),
            Some(0),
        )
        .await
        .expect("the console log")
        .into_inner();
    assert_eq!(console.next, console.start + console.text.len() as i64);

    let worker = admin
        .list_workers()
        .await
        .unwrap()
        .into_inner()
        .into_iter()
        .find(|w| w.name == "w")
        .unwrap();
    let whole = admin
        .get_worker_log(&worker.id, Some(0))
        .await
        .expect("the worker's own log")
        .into_inner();
    assert_eq!(whole.next, whole.start + whole.text.len() as i64);

    let t = admin
        .create_template(&body(json!({
            "name": "Nowhere", "visibility": "private",
            "spec": { "image": "ghcr.io/csnewman/hangar/minimal:ubuntu-26.04", "cpus": 1,
                "memory_mib": 1024, "display": "none", "gpu": "none", "repos": [],
                "placement": { "runtime": "elsewhere" } },
        })))
        .await
        .unwrap()
        .into_inner();
    let nowhere = admin
        .create_environment(&body(json!({ "template_id": t.id, "name": "nowhere" })))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        client_status(
            c.get_environment_log(&nowhere.id, &log("console"), None)
                .await
        ),
        Some(409),
        "an unplaced environment's logs"
    );
}

/// A socket another site opens with a person's cookie is refused.
#[tokio::test]
async fn a_socket_from_another_site_is_refused() {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let Some(s) = Server::start() else { return };
    let _w = s.worker("w");
    let (_, id, token) = running(&s, machine("Origin", "private")).await;
    let tcp = tokio::net::TcpStream::connect(("127.0.0.1", s.port()))
        .await
        .unwrap();
    let mut req = format!(
        "ws://hangar.localhost:{}/api/v1/environments/{id}/code",
        s.port()
    )
    .into_client_request()
    .unwrap();
    req.headers_mut()
        .insert("Authorization", format!("Bearer {token}").parse().unwrap());
    req.headers_mut()
        .insert("Origin", "https://evil.example".parse().unwrap());
    match tokio_tungstenite::client_async(req, tcp).await {
        Err(tokio_tungstenite::tungstenite::Error::Http(r)) => assert_eq!(r.status().as_u16(), 403),
        Ok(_) => panic!("a socket from another site was opened"),
        Err(e) => panic!("opening the socket: {e}"),
    }
}

/// A tunnel nothing crosses for longer than a worker waits to hear from
/// the server stays up: the server keeps it alive.
#[tokio::test]
async fn a_quiet_tunnel_stays_up() {
    let Some(s) = Server::start() else { return };
    let _w = s.worker("w");
    let (_, id, token) = running(&s, machine("Quiet", "private")).await;
    let c = s.client(&token);
    c.list_processes(&id).await.expect("the tunnel at first");
    tokio::time::sleep(std::time::Duration::from_secs(75)).await;
    c.list_processes(&id)
        .await
        .expect("the tunnel after 75 quiet seconds");
}
