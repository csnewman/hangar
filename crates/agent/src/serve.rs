//! `hangar-agent-rs serve`: the agent's services that have moved to Rust,
//! each on its own vsock port, for as long as the guest runs. The Go agent
//! starts it as root, at the agent's own OOM score, and starts it again if
//! it stops.

use std::future::Future;
use std::io;

use crate::terminal;

pub async fn serve() -> io::Result<()> {
    let terminals = terminal::Manager::new(Box::new(terminal::login_shell));
    tokio::spawn(listen("terminals", terminal::PORT, move |conn| {
        let terminals = terminals.clone();
        async move { terminals.serve(conn).await }
    }));
    std::future::pending().await
}

/// Answers every connection to `port`, each on a task of its own, listening
/// again if the socket fails.
#[cfg(target_os = "linux")]
async fn listen<F, Fut>(name: &str, port: u32, handle: F)
where
    F: Fn(crate::fdio::FdStream) -> Fut,
    Fut: Future<Output = ()> + Send + 'static,
{
    use std::time::Duration;

    loop {
        let ln = match crate::vsock::Listener::bind(port) {
            Ok(ln) => ln,
            Err(e) => {
                eprintln!("hangar-agent-rs: {name}: listening on vsock port {port}: {e}");
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
        };
        loop {
            match ln.accept().await {
                Ok(conn) => {
                    tokio::spawn(handle(conn));
                }
                Err(e) => {
                    eprintln!("hangar-agent-rs: {name}: {e}");
                    break;
                }
            }
        }
    }
}

/// Only a Linux guest has vsock.
#[cfg(not(target_os = "linux"))]
async fn listen<F, Fut>(name: &str, _port: u32, _handle: F)
where
    F: Fn(crate::fdio::FdStream) -> Fut,
    Fut: Future<Output = ()> + Send + 'static,
{
    eprintln!("hangar-agent-rs: {name}: no vsock here");
}
