//! Reaching a port inside the guest, through the agent's side of the
//! protocol, from the host's.

use hangar_agent::guestport::{self, CONNECTED, REFUSED};
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
use tokio::net::TcpListener;

/// Asks the agent, on the far end of a pipe, for `port`, returning its
/// answer and the stream.
async fn ask(port: u16) -> (u8, DuplexStream) {
    let (mut host, guest) = tokio::io::duplex(1 << 16);
    tokio::spawn(guestport::serve(guest));
    host.write_all(&port.to_be_bytes()).await.unwrap();
    let mut answer = [0u8; 1];
    host.read_exact(&mut answer).await.unwrap();
    (answer[0], host)
}

#[tokio::test]
async fn connects() {
    let ln = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = ln.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (mut c, _) = ln.accept().await.unwrap();
        c.write_all(b"hello from inside").await.unwrap();
    });
    let (answer, mut host) = ask(port).await;
    assert_eq!(answer, CONNECTED);
    let mut got = String::new();
    host.read_to_string(&mut got).await.unwrap();
    assert_eq!(got, "hello from inside");
}

#[tokio::test]
async fn refused() {
    // A port just released, which nothing listens on.
    let ln = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = ln.local_addr().unwrap().port();
    drop(ln);
    let (answer, _) = ask(port).await;
    assert_eq!(answer, REFUSED);
}
