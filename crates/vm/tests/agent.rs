//! The host end of the control channel, spoken to by the guest agent's own
//! (crates/agent's control::serve).

use std::path::PathBuf;
use std::time::Duration;

use hangar_vm::agent::Server;

fn scratch(name: &str) -> PathBuf {
    let d = PathBuf::from(format!("/tmp/hvm-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[tokio::test]
async fn control_channel() {
    let d = scratch("agent");
    let base = d.join("v");
    let srv = Server::listen_hybrid(&base, 7).unwrap();

    // A connection that ends before its hello is one the agent gave up on.
    drop(
        tokio::net::UnixStream::connect(d.join("v_8100"))
            .await
            .unwrap(),
    );
    let path = d.join("v_8100");
    tokio::spawn(async move {
        let conn = tokio::net::UnixStream::connect(path).await.unwrap();
        hangar_agent::control::serve(conn).await;
    });
    let s = srv.accept(Some(Duration::from_secs(10))).await.unwrap();
    assert_eq!(s.cid, 7);
    assert_eq!(s.hello.kind, "hello");
    assert!(s.hello.page_size >= 4096, "{:?}", s.hello);

    s.ping(Duration::from_secs(5)).await.unwrap();
    let r = s
        .exec(
            Duration::from_secs(10),
            &["/bin/sh", "-c", "echo out; echo err >&2; exit 3"],
        )
        .await
        .unwrap();
    assert_eq!(
        (r.stdout.as_str(), r.stderr.as_str(), r.code),
        ("out\n", "err\n", 3)
    );

    // Several at once, answered as each is done.
    let slow = s.exec(
        Duration::from_secs(10),
        &["/bin/sh", "-c", "sleep 1; echo slow"],
    );
    let quick = s.exec(Duration::from_secs(10), &["/bin/echo", "quick"]);
    let (slow, quick) = tokio::join!(slow, quick);
    assert_eq!(slow.unwrap().stdout, "slow\n");
    assert_eq!(quick.unwrap().stdout, "quick\n");

    let e = s
        .exec(Duration::from_secs(5), &["/no/such/program"])
        .await
        .unwrap_err();
    assert!(e.to_string().contains("/no/such/program"), "{e}");
    let e = s
        .exec(Duration::from_millis(200), &["/bin/sleep", "5"])
        .await
        .unwrap_err();
    assert!(e.to_string().contains("no answer"), "{e}");
    s.ping(Duration::from_secs(5)).await.unwrap();
    let _ = std::fs::remove_dir_all(d);
}
