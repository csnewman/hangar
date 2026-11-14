//! A worker for the server under test: the real one (`hangar_worker`), so
//! its registration, reports, placement and tunnel are the real protocol,
//! on a runtime of the suite's whose machines are what they are asked to be
//! at once, and whose guests answer every stream with a greeting naming its
//! port, then an echo of what they are sent.

use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use hangar_api::{DesiredState, EnvironmentSpec, ObservedEnvironment, Phase};
use hangar_vm::log::Log;
use hangar_worker::config::Config;
use hangar_worker::{Runtime, Worker as RealWorker};
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
use tokio::sync::Notify;

/// The label a template's placement names to land on the suite's workers.
pub const LABEL: (&str, &str) = ("runtime", "fake");

/// A worker, connected to its server until it goes.
pub struct Worker {
    task: Option<tokio::task::JoinHandle<Result<(), String>>>,
    machines: Machines,
    dir: PathBuf,
}

impl Worker {
    /// Starts a worker named name for the server at url (its own address:
    /// the worker reaches it by number), presenting the bootstrap token.
    pub fn start(url: &str, name: &str) -> Worker {
        Worker::start_with_token(url, name, crate::BOOTSTRAP_TOKEN)
    }

    /// As start, presenting token as the bootstrap token.
    pub fn start_with_token(url: &str, name: &str, token: &str) -> Worker {
        let dir = std::env::temp_dir().join(crate::unique("hangar-suite-worker"));
        std::fs::create_dir_all(&dir).expect("the worker's directory");
        std::fs::write(dir.join("token"), format!("{token}\n")).expect("the bootstrap token");
        let cfg = Config::parse(&format!(
            "server:\n  url: {url}\n  token_file: {}\n  credential_file: {}\n\
             node:\n  name: {name}\n  labels:\n    {}: {}\n\
             storage:\n  environments: {}\n  files: {}\n\
             runtime: simulated\n",
            dir.join("token").display(),
            dir.join("credential").display(),
            LABEL.0,
            LABEL.1,
            dir.join("env").display(),
            dir.join("files").display(),
        ))
        .expect("the worker's configuration");
        let machines = Machines::default();
        let worker = RealWorker::new(cfg, machines.clone(), Log::default(), None)
            .expect("making the worker");
        let task = tokio::spawn(async move { worker.run().await.map_err(|e| e.to_string()) });
        Worker {
            task: Some(task),
            machines,
            dir,
        }
    }

    /// How the worker's run ended, if it ends within 20 seconds: an error
    /// says why, such as the server rejecting its credential.
    pub async fn ended(&mut self) -> Option<Result<(), String>> {
        let task = self.task.as_mut()?;
        let out = tokio::time::timeout(std::time::Duration::from_secs(20), task)
            .await
            .ok()?;
        self.task = None;
        Some(out.unwrap_or_else(|e| Err(e.to_string())))
    }

    /// What the worker has been asked of an environment, last.
    pub fn desired(&self, id: &str) -> Option<DesiredState> {
        self.machines.desired(id)
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Machines that are as they are asked to be.
#[derive(Clone, Default)]
struct Machines {
    held: Arc<Mutex<BTreeMap<String, DesiredState>>>,
    changed: Arc<Notify>,
}

impl Machines {
    fn desired(&self, id: &str) -> Option<DesiredState> {
        self.held.lock().unwrap().get(id).copied()
    }
}

impl Runtime for Machines {
    type Conn = DuplexStream;

    fn apply(&self, spec: EnvironmentSpec) {
        let mut held = self.held.lock().unwrap();
        if spec.desired == DesiredState::Deleted {
            held.remove(&spec.id);
        } else {
            held.insert(spec.id, spec.desired);
        }
        drop(held);
        self.changed.notify_one();
    }

    fn observe(&self) -> Vec<ObservedEnvironment> {
        self.held
            .lock()
            .unwrap()
            .iter()
            .map(|(id, desired)| ObservedEnvironment {
                id: id.clone(),
                phase: match desired {
                    DesiredState::Running => Phase::Running,
                    DesiredState::Suspended => Phase::Suspended,
                    _ => Phase::Stopped,
                },
                reason: String::new(),
                progress: None,
                stats: None,
                image_digest: String::new(),
                image_pinned: false,
                image_update: None,
                image_rollback: None,
            })
            .collect()
    }

    async fn changed(&self) {
        self.changed.notified().await
    }

    async fn dial(&self, id: &str, port: u32) -> io::Result<DuplexStream> {
        if self.desired(id) != Some(DesiredState::Running) {
            return Err(io::Error::other(format!("environment {id} is not running")));
        }
        let (ours, mut guest) = tokio::io::duplex(1 << 16);
        tokio::spawn(async move {
            let _ = guest.write_all(format!("port {port}\n").as_bytes()).await;
            let mut buf = [0u8; 4096];
            loop {
                match guest.read(&mut buf).await {
                    Ok(0) | Err(_) => return,
                    Ok(n) => {
                        if guest.write_all(&buf[..n]).await.is_err() {
                            return;
                        }
                    }
                }
            }
        });
        Ok(ours)
    }

    /// It says it is libkrun, a monitor the spec knows, and that it
    /// suspends, so the server lets environments be suspended on it.
    fn monitor(&self) -> (String, bool) {
        ("krun".into(), true)
    }
}
