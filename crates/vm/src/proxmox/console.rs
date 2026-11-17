//! A Proxmox VM's serial console, copied into its console log: through
//! the API's terminal proxy (`termproxy`) and its websocket, which is how
//! Proxmox's own page shows it. The serial port has one reader at a time,
//! so while the worker follows it, Proxmox's page shows the VM's display
//! and not its serial console.

use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::io::AsyncWriteExt;
use tokio_tungstenite::tungstenite::Message;

use super::Pve;
use super::client::enc;

/// How long to keep trying to attach before the VM has been seen running.
const ATTACH_WITHIN: Duration = Duration::from_secs(120);
/// How often the proxy is told the reader is still there, as Proxmox's
/// own page does.
const PING_EVERY: Duration = Duration::from_secs(30);

/// Follows a VM's serial console into the file at out until the VM stops,
/// or the task is aborted: a new file if fresh, as for a start, else added
/// to, as for a VM taken back as it runs. It starts before the VM does,
/// attaching as soon as Proxmox lets it, so the log has the guest's first
/// lines.
pub fn follow(pve: Arc<Pve>, id: u32, out: PathBuf, fresh: bool) -> Following {
    Following(tokio::spawn(async move {
        let log = pve.log.clone();
        if let Err(e) = run(&pve, id, &out, fresh).await {
            log.debug("following the VM's console", &[("vmid", &id), ("err", &e)]);
        }
    }))
}

/// The console being followed; dropping it stops.
pub struct Following(tokio::task::JoinHandle<()>);

impl Drop for Following {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn run(pve: &Pve, id: u32, out: &PathBuf, fresh: bool) -> io::Result<()> {
    // A new file, as each start's console log is, so a reader following
    // the last knows this is another.
    if fresh {
        match tokio::fs::remove_file(out).await {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
    }
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(out)
        .await?;
    let deadline = tokio::time::Instant::now() + ATTACH_WITHIN;
    let mut attached = false;
    loop {
        match attach(pve, id, &mut file).await {
            Ok(()) => attached = true,
            Err(e) if !attached && tokio::time::Instant::now() > deadline => return Err(e),
            Err(_) => {}
        }
        // The proxy ends when the VM stops, or resumes from a hibernate.
        if attached && !pve.running(id).await.unwrap_or(false) {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// One session of the terminal proxy, copied into file until it ends.
async fn attach(pve: &Pve, id: u32, file: &mut tokio::fs::File) -> io::Result<()> {
    let proxy = pve
        .client
        .post(&pve.vm(id, "termproxy"), &[("serial", "serial0")])
        .await?;
    let port = proxy["port"]
        .as_u64()
        .or_else(|| proxy["port"].as_str().and_then(|p| p.parse().ok()))
        .ok_or_else(|| crate::err("termproxy gave no port"))?;
    let ticket = proxy["ticket"].as_str().unwrap_or_default();
    let user = proxy["user"].as_str().unwrap_or_default();
    let path = format!(
        "{}?port={port}&vncticket={}",
        pve.vm(id, "vncwebsocket"),
        enc(ticket)
    );
    let mut ws = pve.client.websocket(&path).await?;
    ws.send(Message::binary(format!("{user}:{ticket}\n").into_bytes()))
        .await
        .map_err(io::Error::other)?;
    let mut ping = tokio::time::interval(PING_EVERY);
    ping.tick().await;
    let mut greeted = false;
    loop {
        tokio::select! {
            m = ws.next() => {
                let data = match m {
                    Some(Ok(Message::Binary(b))) => b.to_vec(),
                    Some(Ok(Message::Text(t))) => t.as_bytes().to_vec(),
                    Some(Ok(Message::Close(_))) | None => return Ok(()),
                    Some(Ok(_)) => continue,
                    Some(Err(e)) => return Err(io::Error::other(e)),
                };
                // The proxy's first answer is its "OK" to the ticket.
                let data = if greeted {
                    &data[..]
                } else {
                    greeted = true;
                    data.strip_prefix(b"OK").unwrap_or(&data)
                };
                file.write_all(data).await?;
                file.flush().await?;
            }
            _ = ping.tick() => {
                ws.send(Message::binary(b"2".to_vec())).await.map_err(io::Error::other)?;
            }
        }
    }
}
