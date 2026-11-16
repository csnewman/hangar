//! An agent and a worker over the hub: TLS to the pinned certificate alone,
//! the Hello and its answer, and streams opened by either end, many at once,
//! each carrying its own bytes.

use std::time::Duration;

use hangar_hub::{Acceptor, Hello, Incoming, Link, connect, tls};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const SECRET: &str = "s3cret";

/// A worker's hub on the loopback that lets in environment e1 with SECRET,
/// and its certificate's fingerprint.
async fn worker() -> (
    String,
    String,
    tokio::sync::mpsc::Receiver<(Link, Incoming)>,
) {
    let (cert, key) = tls::generate().unwrap();
    let fp = tls::fingerprint(&cert);
    let acceptor = Acceptor::new(cert, key).unwrap();
    let ln = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = ln.local_addr().unwrap().to_string();
    let (tx, rx) = tokio::sync::mpsc::channel(4);
    tokio::spawn(async move {
        loop {
            let (tcp, _) = ln.accept().await.unwrap();
            let (acceptor, tx) = (acceptor.clone(), tx.clone());
            tokio::spawn(async move {
                let Ok(g) = acceptor.hello(tcp, Duration::from_secs(5)).await else {
                    return;
                };
                if g.hello.environment != "e1" || g.hello.secret != SECRET {
                    g.refuse("unknown environment or secret").await;
                    return;
                }
                let _ = tx.send(g.welcome().await.unwrap()).await;
            });
        }
    });
    (addr, fp, rx)
}

fn hello(secret: &str) -> Hello {
    Hello {
        environment: "e1".into(),
        secret: secret.into(),
        agent: "test".into(),
    }
}

#[tokio::test]
async fn either_end_opens_streams_by_port() {
    let (addr, fp, mut links) = worker().await;
    let (agent, mut agent_in) = connect(&addr, &fp, &hello(SECRET)).await.unwrap();
    let (worker, mut worker_in) = links.recv().await.unwrap();

    // The agent dials the worker's control channel.
    let mut s = agent.open(8100).await.unwrap();
    s.write_all(b"hello worker").await.unwrap();
    s.flush().await.unwrap();
    let (port, mut got) = worker_in.next().await.unwrap().open().await.unwrap();
    assert_eq!(port, 8100);
    let mut buf = [0u8; 12];
    got.read_exact(&mut buf).await.unwrap();
    assert_eq!(&buf, b"hello worker");

    // The worker dials the agent's terminals.
    let mut t = worker.open(101).await.unwrap();
    t.write_all(b"ls\n").await.unwrap();
    t.flush().await.unwrap();
    let (port, mut got) = agent_in.next().await.unwrap().open().await.unwrap();
    assert_eq!(port, 101);
    let mut buf = [0u8; 3];
    got.read_exact(&mut buf).await.unwrap();
    assert_eq!(&buf, b"ls\n");
    got.write_all(b"ok").await.unwrap();
    got.flush().await.unwrap();
    let mut buf = [0u8; 2];
    t.read_exact(&mut buf).await.unwrap();
    assert_eq!(&buf, b"ok");
}

#[tokio::test]
async fn another_certificate_is_refused() {
    let (addr, _, _links) = worker().await;
    let (other, _) = tls::generate().unwrap();
    let err = connect(&addr, &tls::fingerprint(&other), &hello(SECRET))
        .await
        .err()
        .expect("a certificate other than the pinned one");
    assert!(err.to_string().contains("not the one expected"), "{err}");
    assert!(
        connect(&addr, "not hex", &hello(SECRET)).await.is_err(),
        "a fingerprint that is not one"
    );
}

#[tokio::test]
async fn a_wrong_secret_is_turned_away_with_the_reason() {
    let (addr, fp, _links) = worker().await;
    let err = connect(&addr, &fp, &hello("guess")).await.err().unwrap();
    assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
    assert_eq!(err.to_string(), "unknown environment or secret");
}

#[tokio::test]
async fn a_link_closes_when_the_other_end_goes() {
    let (addr, fp, mut links) = worker().await;
    let (agent, agent_in) = connect(&addr, &fp, &hello(SECRET)).await.unwrap();
    let (worker, mut worker_in) = links.recv().await.unwrap();
    assert!(!worker.is_closed());
    drop((agent, agent_in));
    tokio::time::timeout(Duration::from_secs(5), worker.closed())
        .await
        .expect("the worker's end closes");
    assert!(worker.is_closed());
    assert!(worker_in.next().await.is_none(), "no more streams");
    assert!(worker.open(101).await.is_err(), "and none can be opened");
}

/// Many streams at once, each echoing a megabyte of its own, as terminals,
/// the Code service and shared files share one link.
#[tokio::test]
async fn many_streams_carry_their_own_bytes() {
    let (addr, fp, mut links) = worker().await;
    let (agent, mut agent_in) = connect(&addr, &fp, &hello(SECRET)).await.unwrap();
    let (worker, _worker_in) = links.recv().await.unwrap();
    tokio::spawn(async move {
        while let Some(a) = agent_in.next().await {
            tokio::spawn(async move {
                let (_, mut s) = a.open().await.unwrap();
                let mut buf = vec![0u8; 64 << 10];
                loop {
                    let n = s.read(&mut buf).await.unwrap();
                    if n == 0 {
                        break;
                    }
                    s.write_all(&buf[..n]).await.unwrap();
                }
                s.shutdown().await.unwrap();
            });
        }
    });
    let _agent = agent;
    let mut tasks = vec![];
    for i in 0..32u32 {
        let worker = worker.clone();
        tasks.push(tokio::spawn(async move {
            let s = worker.open(1000 + i).await.unwrap();
            let data: Vec<u8> = (0..1 << 20).map(|j| (j as u32 ^ i) as u8).collect();
            let (mut r, mut w) = tokio::io::split(s);
            let sent = data.clone();
            let writer = async move {
                w.write_all(&sent).await.unwrap();
                w.shutdown().await.unwrap();
            };
            let reader = async move {
                let mut back = Vec::new();
                r.read_to_end(&mut back).await.unwrap();
                back
            };
            let ((), back) = tokio::join!(writer, reader);
            assert_eq!(back.len(), data.len(), "stream {i}");
            assert!(back == data, "stream {i}'s bytes");
        }));
    }
    for t in tasks {
        tokio::time::timeout(Duration::from_secs(60), t)
            .await
            .expect("in time")
            .unwrap();
    }
}

/// Closing a link ends it at both ends, every stream on it with it.
#[tokio::test]
async fn a_link_closed_ends_at_both_ends() {
    let (addr, fp, mut links) = worker().await;
    let (agent, _agent_in) = connect(&addr, &fp, &hello(SECRET)).await.unwrap();
    let (worker, _worker_in) = links.recv().await.unwrap();
    let mut s = agent.open(8100).await.unwrap();
    worker.close();
    tokio::time::timeout(Duration::from_secs(5), agent.closed())
        .await
        .expect("the agent's end closes");
    let mut buf = [0u8; 1];
    let n = s.read(&mut buf).await.unwrap_or(0);
    assert_eq!(n, 0, "and its streams end");
}

/// A relay whose stream has spent its window on a peer that stops reading
/// ends when the link closes, letting its connection go: the stream itself
/// is not woken.
#[tokio::test]
async fn a_relay_ends_with_its_link() {
    let (addr, fp, mut links) = worker().await;
    let (agent, _agent_in) = connect(&addr, &fp, &hello(SECRET)).await.unwrap();
    let (_worker, mut worker_in) = links.recv().await.unwrap();
    let s = agent.open(8100).await.unwrap();
    // The worker takes the stream and never reads it.
    let held = tokio::spawn(async move {
        let (_, stream) = worker_in.next().await.unwrap().open().await.unwrap();
        tokio::time::sleep(Duration::from_secs(3600)).await;
        drop(stream);
    });
    let (mut here, there) = tokio::net::UnixStream::pair().unwrap();
    let relaying = tokio::spawn(agent.relay(s, there));
    // Far more than a stream's window and the sockets' buffers between.
    let writing = tokio::spawn(async move {
        let chunk = vec![7u8; 64 * 1024];
        for _ in 0..256 {
            if here.write_all(&chunk).await.is_err() {
                break;
            }
        }
        here
    });
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(!relaying.is_finished(), "the relay is held by the stream");
    agent.close();
    tokio::time::timeout(Duration::from_secs(5), relaying)
        .await
        .expect("the relay ends once its link has closed")
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), writing)
        .await
        .expect("and its connection is let go")
        .unwrap();
    held.abort();
}
