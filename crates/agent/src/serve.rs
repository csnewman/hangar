//! The agent's services, each on its own vsock port -- terminals, the
//! desktop, the Code service, SSH, the profile session and what it carries,
//! processes, the guest's ports -- and the handing back of the guest's
//! memory, for as long as the guest runs. hangar-agent.service runs the
//! agent as root, at OOM score -1000 (`init::OOM_DROP_IN`).

use std::future::Future;
use std::io;
use std::sync::Arc;

use crate::{codeservice, desktop, guestport, memory, procs, profile, sshd, terminal};

pub async fn serve() -> io::Result<()> {
    memory::widen_reclaim();
    tokio::spawn(memory::offload());
    tokio::spawn(memory::compact());
    let terminals = terminal::Manager::new(Box::new(terminal::login_shell));
    tokio::spawn(listen("terminals", terminal::PORT, move |conn| {
        let terminals = terminals.clone();
        async move { terminals.serve(conn).await }
    }));
    let desktop = Arc::new(desktop::Server::new());
    tokio::spawn(listen("desktop", desktop::PORT, move |conn| {
        let desktop = desktop.clone();
        async move { desktop.serve(conn).await }
    }));
    let code = Arc::new(codeservice::Server::new());
    tokio::spawn(listen("code", codeservice::PORT, move |conn| {
        let code = code.clone();
        async move { code.serve(conn).await }
    }));
    let procs = Arc::new(procs::Server::new());
    tokio::spawn(listen("procs", procs::PORT, move |conn| {
        let procs = procs.clone();
        async move { procs.serve(conn).await }
    }));
    tokio::spawn(listen("ports", guestport::PORT, guestport::serve));
    match sshd::Server::new(crate::user::DEFAULT_USER) {
        Ok(ssh) => {
            let ssh = Arc::new(ssh);
            tokio::spawn(listen("ssh", sshd::PORT, move |conn| {
                let ssh = ssh.clone();
                async move { ssh.serve(conn).await }
            }));
        }
        Err(e) => eprintln!("hangar-agent: ssh: {e}"),
    }
    tokio::spawn(profile::serve());
    std::future::pending().await
}

/// Answers every connection to `port`, each on a task of its own, listening
/// again if the socket fails.
#[cfg(target_os = "linux")]
pub(crate) async fn listen<F, Fut>(name: &str, port: u32, handle: F)
where
    F: Fn(crate::fdio::FdStream) -> Fut,
    Fut: Future<Output = ()> + Send + 'static,
{
    use std::time::Duration;

    loop {
        let ln = match crate::vsock::Listener::bind(port) {
            Ok(ln) => ln,
            Err(e) => {
                eprintln!("hangar-agent: {name}: listening on vsock port {port}: {e}");
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
                    eprintln!("hangar-agent: {name}: {e}");
                    break;
                }
            }
        }
    }
}

/// Only a Linux guest has vsock.
#[cfg(not(target_os = "linux"))]
pub(crate) async fn listen<F, Fut>(name: &str, _port: u32, _handle: F)
where
    F: Fn(crate::fdio::FdStream) -> Fut,
    Fut: Future<Output = ()> + Send + 'static,
{
    eprintln!("hangar-agent: {name}: no vsock here");
}

/// A task stopped when this is dropped.
pub(crate) struct AbortOnDrop<T>(pub tokio::task::JoinHandle<T>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}
