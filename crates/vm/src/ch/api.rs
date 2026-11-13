//! A running monitor, driven through its API socket.
//!
//! It speaks the monitor's HTTP API directly rather than running
//! ch-remote, so suspending an environment needs nothing installed beyond
//! the monitor itself.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use super::config::plugged;
use crate::err;

pub struct Api {
    socket: PathBuf,
}

/// A running guest's memory as its monitor has it, in MiB.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Memory {
    /// What the guest booted with, and what it can be resized up to; they
    /// are the same for a guest whose memory cannot be resized.
    pub boot: u32,
    pub max: u32,
    /// What it was last asked to have, and what it has: the guest plugs
    /// memory in and gives it back in its own time, and may not manage to
    /// give back all it is asked to.
    pub requested: u32,
    pub actual: u32,
}

impl Memory {
    /// Whether the guest's memory can be resized while it runs.
    pub fn resizable(&self) -> bool {
        self.max > self.boot
    }

    /// The size nearest mb the guest can be resized to: between boot and
    /// max, in whole blocks above boot.
    pub fn fit(&self, mb: u32) -> u32 {
        if !self.resizable() {
            return self.boot;
        }
        self.boot
            + plugged(
                mb.min(self.max) as i64 - self.boot as i64,
                self.max - self.boot,
            )
    }
}

impl Api {
    /// A client for the monitor listening on socket.
    pub fn new(socket: impl AsRef<Path>) -> Api {
        Api {
            socket: socket.as_ref().to_path_buf(),
        }
    }

    /// Waits for the monitor to accept connections on its API socket.
    pub async fn wait_ready(&self, timeout: Duration) -> io::Result<()> {
        let deadline = tokio::time::Instant::now() + timeout;
        while tokio::time::Instant::now() < deadline {
            // The socket file exists from the moment the monitor binds it,
            // which is before it listens; only a connection that is
            // accepted shows it is ready for requests.
            if let Ok(Ok(_)) =
                tokio::time::timeout(Duration::from_secs(1), UnixStream::connect(&self.socket))
                    .await
            {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        Err(err(format!(
            "the monitor did not accept connections on {} within {timeout:?}",
            self.socket.display()
        )))
    }

    /// Makes one request, giving the response's body.
    async fn request(
        &self,
        method: &str,
        action: &str,
        body: Option<serde_json::Value>,
    ) -> io::Result<Vec<u8>> {
        let fail = |e: io::Error| err(format!("{action}: {e}"));
        let mut conn = UnixStream::connect(&self.socket).await.map_err(fail)?;
        let body = body.map(|b| b.to_string()).unwrap_or_default();
        let mut req =
            format!("{method} /api/v1/{action} HTTP/1.1\r\nHost: localhost\r\nAccept: */*\r\n");
        if !body.is_empty() {
            req += "Content-Type: application/json\r\n";
        }
        req += &format!("Content-Length: {}\r\n\r\n{body}", body.len());
        conn.write_all(req.as_bytes()).await.map_err(fail)?;

        let mut r = BufReader::new(conn);
        let mut status = String::new();
        r.read_line(&mut status).await.map_err(fail)?;
        let code: u16 = status
            .split_whitespace()
            .nth(1)
            .and_then(|c| c.parse().ok())
            .ok_or_else(|| err(format!("{action}: a response that is not HTTP: {status:?}")))?;
        let mut length = 0usize;
        loop {
            let mut line = String::new();
            if r.read_line(&mut line).await.map_err(fail)? == 0 {
                break;
            }
            let line = line.trim_end();
            if line.is_empty() {
                break;
            }
            if let Some((k, v)) = line.split_once(':')
                && k.eq_ignore_ascii_case("content-length")
            {
                length = v.trim().parse().unwrap_or(0);
            }
        }
        let mut out = vec![0u8; length];
        r.read_exact(&mut out).await.map_err(fail)?;
        if code / 100 != 2 {
            return Err(err(format!(
                "{action}: {}: {}",
                status.trim_start_matches("HTTP/1.1 ").trim(),
                String::from_utf8_lossy(&out).trim()
            )));
        }
        Ok(out)
    }

    async fn put(&self, action: &str, body: Option<serde_json::Value>) -> io::Result<()> {
        self.request("PUT", action, body).await.map(|_| ())
    }

    /// Stops the guest's vCPUs and its devices' guest traffic.
    pub async fn pause(&self) -> io::Result<()> {
        self.put("vm.pause", None).await
    }

    /// Starts a paused guest again.
    pub async fn resume(&self) -> io::Result<()> {
        self.put("vm.resume", None).await
    }

    /// Writes a paused guest's memory and device state into dir.
    pub async fn snapshot(&self, dir: &Path) -> io::Result<()> {
        let url = format!("file://{}", dir.display());
        self.put(
            "vm.snapshot",
            Some(serde_json::json!({ "destination_url": url })),
        )
        .await
    }

    /// Rebuilds a guest from a snapshot in dir. It is left paused.
    pub async fn restore(&self, dir: &Path) -> io::Result<()> {
        let url = format!("file://{}", dir.display());
        self.put("vm.restore", Some(serde_json::json!({ "source_url": url })))
            .await
    }

    /// Stops the monitor process.
    pub async fn shutdown(&self) -> io::Result<()> {
        self.put("vmm.shutdown", None).await
    }

    /// The guest's memory.
    pub async fn memory(&self) -> io::Result<Memory> {
        #[derive(Deserialize)]
        struct Info {
            config: InfoConfig,
            memory_actual_size: i64,
        }
        #[derive(Deserialize)]
        struct InfoConfig {
            memory: InfoMemory,
        }
        #[derive(Deserialize)]
        struct InfoMemory {
            size: i64,
            hotplug_size: Option<i64>,
            hotplugged_size: Option<i64>,
        }
        let body = self.request("GET", "vm.info", None).await?;
        let info: Info = serde_json::from_slice(&body).map_err(|e| err(format!("vm.info: {e}")))?;
        let mb = |b: Option<i64>| (b.unwrap_or(0) >> 20) as u32;
        let m = info.config.memory;
        let boot = (m.size >> 20) as u32;
        Ok(Memory {
            boot,
            max: boot + mb(m.hotplug_size),
            requested: boot + mb(m.hotplugged_size),
            actual: (info.memory_actual_size >> 20) as u32,
        })
    }

    /// Asks the guest to have mb MiB, which must be a size Memory::fit
    /// gives. It returns once asked; memory says how far the guest has got.
    pub async fn resize_memory(&self, mb: u32) -> io::Result<()> {
        self.put(
            "vm.resize",
            Some(serde_json::json!({ "desired_ram": (mb as i64) << 20 })),
        )
        .await
    }
}
