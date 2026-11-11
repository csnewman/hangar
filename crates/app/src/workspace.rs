//! An environment's workspace: its projects (the workspace file's
//! folders), each with a Code connection of its own, as the web UI's Code
//! tab has, and the connection in its editor folder that reads the
//! workspace and the machine's files. The tool windows showing projects
//! share one, and hear the service's events through it.

use gpui::{Context, Entity, EventEmitter, WeakEntity};
use serde_json::{Value, json};

use crate::code::{Code, Notice};
use crate::places::Places;

#[derive(Debug, Clone, PartialEq)]
pub enum State {
    Connecting,
    Ready,
    /// The editor folder's connection was lost, and is being made again:
    /// why.
    Reconnecting(String),
    Failed(String),
}

/// A project: a folder of the workspace.
pub struct Folder {
    pub root: String,
    pub name: String,
    pub code: Code,
}

/// What the workspace tells the views showing it.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// The projects, or the workspace's state, changed.
    Projects,
    /// One of the service's events, from a project's connection (its root)
    /// or the editor folder's (PRIMARY).
    Heard {
        root: String,
        event: String,
        params: Value,
    },
    /// A project's connection (its root) or the editor folder's (PRIMARY)
    /// was lost: what was started on it, its language servers, is gone.
    /// Projects follows once it is made again.
    Lost { root: String },
    /// A language server's message, on a project's connection.
    Lsp {
        root: String,
        server: u32,
        message: Value,
    },
}

impl EventEmitter<Event> for Workspace {}

/// The editor folder's connection among the events; a project's is its
/// root.
pub const PRIMARY: &str = "";

impl State {
    /// What a view says in place of the projects while the service is
    /// out of reach.
    pub fn reconnecting(why: &str) -> String {
        format!("Reconnecting to the Code service ({why})…")
    }
}

pub struct Workspace {
    places: Entity<Places>,
    place: String,
    env: String,
    state: State,
    primary: Option<Code>,
    home: String,
    /// The Code settings file, as the service says at hello.
    settings: String,
    projects: Vec<Folder>,
    /// The connections lost and not yet made again, by key.
    lost: std::collections::HashSet<String>,
    notices: async_channel::Sender<(String, Notice)>,
}

impl Workspace {
    /// Connects to the environment's Code service, from its editor folder.
    pub fn new(
        places: Entity<Places>,
        place: &str,
        env: &str,
        editor: &str,
        cx: &mut Context<Self>,
    ) -> Self {
        let (tx, rx) = async_channel::unbounded::<(String, Notice)>();
        cx.spawn(async move |this: WeakEntity<Self>, cx| {
            while let Ok((key, notice)) = rx.recv().await {
                if this.update(cx, |w, cx| w.heard(key, notice, cx)).is_err() {
                    return;
                }
            }
        })
        .detach();
        let mut w = Workspace {
            places,
            place: place.into(),
            env: env.into(),
            state: State::Connecting,
            primary: None,
            home: String::new(),
            settings: String::new(),
            projects: vec![],
            lost: Default::default(),
            notices: tx,
        };
        w.primary = w.connect(PRIMARY, editor, cx);
        if w.primary.is_none() {
            w.state = State::Failed("This place has no connection.".into());
        }
        w
    }

    fn connect(&self, key: &str, root: &str, cx: &Context<Self>) -> Option<Code> {
        let client = self.places.read(cx).client(&self.place)?;
        let tx = self.notices.clone();
        let key = key.to_string();
        Some(Code::connect(&client, &self.env, root, move |n| {
            tx.send_blocking((key.clone(), n)).is_ok()
        }))
    }

    pub fn state(&self) -> &State {
        &self.state
    }

    pub fn projects(&self) -> &[Folder] {
        &self.projects
    }

    /// Makes the projects the folders given, by absolute path, in order: the
    /// workspace file the service keeps, which every tab follows.
    pub fn set_folders(
        &self,
        folders: Vec<String>,
    ) -> impl std::future::Future<Output = Result<Value, String>> + use<> {
        let call = self
            .primary
            .as_ref()
            .map(|c| c.call("workspace.set", json!({ "folders": folders })));
        async move {
            match call {
                Some(c) => c.await,
                None => Err("not connected to the Code service".into()),
            }
        }
    }

    /// The projects' roots, in order.
    pub fn roots(&self) -> Vec<String> {
        self.projects.iter().map(|f| f.root.clone()).collect()
    }

    /// A project's connection, by its root.
    pub fn code(&self, root: &str) -> Option<Code> {
        self.projects
            .iter()
            .find(|f| f.root == root)
            .map(|f| f.code.clone())
    }

    /// The editor folder's connection, which reaches the whole machine.
    pub fn primary(&self) -> Option<Code> {
        self.primary.clone()
    }

    /// The project a file is in, by its absolute path: the innermost.
    pub fn project_of(&self, abs: &str) -> Option<&Folder> {
        self.projects
            .iter()
            .filter(|f| abs.starts_with(&format!("{}/", f.root.trim_end_matches('/'))))
            .max_by_key(|f| f.root.len())
    }

    /// The user's home folder, once connected.
    pub fn home(&self) -> Option<&str> {
        Some(self.home.as_str()).filter(|h| !h.is_empty())
    }

    /// The Code settings file, once connected.
    pub fn settings_path(&self) -> Option<&str> {
        Some(self.settings.as_str()).filter(|s| !s.is_empty())
    }

    /// The path shown for a folder: under the home folder as ~.
    pub fn shown(&self, root: &str) -> String {
        match root.strip_prefix(&self.home) {
            Some(rest) if !self.home.is_empty() => format!("~{rest}"),
            _ => root.to_string(),
        }
    }

    fn heard(&mut self, key: String, notice: Notice, cx: &mut Context<Self>) {
        match notice {
            Notice::Ready(hello) if key == PRIMARY => {
                self.lost.remove(&key);
                self.home = hello["home"].as_str().unwrap_or_default().to_string();
                self.settings = hello["settings"].as_str().unwrap_or_default().to_string();
                let root = hello["root"].as_str().unwrap_or_default().to_string();
                self.read(&root, cx);
            }
            Notice::Event(e, params) if key == PRIMARY && e == "workspace.changed" => {
                self.set_projects(folders(&params), cx);
            }
            Notice::Closed(why) if key == PRIMARY => {
                self.state = State::Failed(why);
                cx.emit(Event::Projects);
                cx.notify();
            }
            Notice::Lost { why, .. } => {
                if key == PRIMARY {
                    self.state = State::Reconnecting(why);
                    cx.emit(Event::Projects);
                    cx.notify();
                }
                self.lost.insert(key.clone());
                cx.emit(Event::Lost { root: key });
            }
            // A project's connection made again: what the views show of it
            // is read again.
            Notice::Ready(_) if self.lost.remove(&key) => {
                cx.emit(Event::Projects);
                cx.notify();
            }
            Notice::Event(event, params) => cx.emit(Event::Heard {
                root: key,
                event,
                params,
            }),
            Notice::Lsp(server, message) => cx.emit(Event::Lsp {
                root: key,
                server,
                message,
            }),
            _ => {}
        }
    }

    /// Reads the workspace's projects through the primary connection.
    fn read(&mut self, primary_root: &str, cx: &mut Context<Self>) {
        let Some(code) = self.primary.clone() else {
            return;
        };
        let call = code.call("workspace.get", json!({}));
        let fallback = primary_root.to_string();
        cx.spawn(async move |this, cx| {
            let result = call.await;
            let _ = this.update(cx, |w, cx| match result {
                Ok(v) => {
                    let mut f = folders(&v);
                    if f.is_empty() && !fallback.is_empty() {
                        f.push(fallback);
                    }
                    w.state = State::Ready;
                    w.set_projects(f, cx);
                }
                Err(e) => {
                    w.state = State::Failed(e);
                    cx.emit(Event::Projects);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Makes the projects the folders, keeping those already connected.
    fn set_projects(&mut self, folders: Vec<String>, cx: &mut Context<Self>) {
        let mut old = std::mem::take(&mut self.projects);
        for root in folders {
            if let Some(i) = old.iter().position(|f| f.root == root) {
                self.projects.push(old.remove(i));
                continue;
            }
            let Some(code) = self.connect(&root, &root, cx) else {
                continue;
            };
            let name = root
                .rsplit('/')
                .next()
                .filter(|n| !n.is_empty())
                .unwrap_or(&root)
                .to_string();
            self.projects.push(Folder { root, name, code });
        }
        cx.emit(Event::Projects);
        cx.notify();
    }
}

/// The folders a workspace (or its change event) names.
fn folders(v: &Value) -> Vec<String> {
    v["folders"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|f| f.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}
