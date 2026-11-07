//! Language servers: which to start for a language, and one running, its
//! messages framed as LSP frames them on its stdio.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Map, Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

use super::child;

/// A language server Hangar knows how to start: its command, the arguments
/// that have it speak LSP on stdin and stdout, the LSP language IDs it
/// serves, and the usual way to install it. The first one installed for a
/// language is the one used.
#[derive(Clone)]
pub struct Spec {
    pub command: String,
    pub args: Vec<String>,
    install: Option<String>,
}

struct Known {
    command: &'static str,
    args: &'static [&'static str],
    languages: &'static [&'static str],
    install: Option<&'static str>,
}

const KNOWN: &[Known] = &[
    Known {
        command: "gopls",
        args: &[],
        languages: &["go"],
        install: Some("go install golang.org/x/tools/gopls@latest"),
    },
    Known {
        command: "typescript-language-server",
        args: &["--stdio"],
        languages: &[
            "typescript",
            "typescriptreact",
            "javascript",
            "javascriptreact",
        ],
        install: Some("npm install -g typescript-language-server typescript"),
    },
    Known {
        command: "pyright-langserver",
        args: &["--stdio"],
        languages: &["python"],
        install: Some("npm install -g pyright"),
    },
    Known {
        command: "basedpyright-langserver",
        args: &["--stdio"],
        languages: &["python"],
        install: Some("pip install basedpyright"),
    },
    Known {
        command: "pylsp",
        args: &[],
        languages: &["python"],
        install: Some("pip install python-lsp-server"),
    },
    Known {
        command: "rust-analyzer",
        args: &[],
        languages: &["rust"],
        install: Some("rustup component add rust-analyzer"),
    },
    Known {
        command: "clangd",
        args: &[],
        languages: &["c", "cpp"],
        install: Some("sudo apt-get install -y clangd"),
    },
    Known {
        command: "bash-language-server",
        args: &["start"],
        languages: &["shellscript"],
        install: Some("npm install -g bash-language-server"),
    },
    Known {
        command: "yaml-language-server",
        args: &["--stdio"],
        languages: &["yaml"],
        install: Some("npm install -g yaml-language-server"),
    },
    Known {
        command: "vscode-json-language-server",
        args: &["--stdio"],
        languages: &["json"],
        install: Some("npm install -g vscode-langservers-extracted"),
    },
    Known {
        command: "jdtls",
        args: &[],
        languages: &["java"],
        install: None,
    },
    Known {
        command: "zls",
        args: &[],
        languages: &["zig"],
        install: None,
    },
];

/// The service's PATH is the system's; language servers are as often in
/// the user's own directories, where their installers put them.
fn search_path() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    dirs.extend([
        home.join("go/bin"),
        home.join(".local/bin"),
        home.join(".cargo/bin"),
        home.join(".npm-global/bin"),
        PathBuf::from("/usr/local/go/bin"),
    ]);
    dirs.retain(|d| !d.as_os_str().is_empty());
    dirs
}

fn which(command: &str) -> Option<PathBuf> {
    search_path()
        .into_iter()
        .map(|d| d.join(command))
        .find(|p| executable(p))
}

fn executable(p: &Path) -> bool {
    let Ok(c) = std::ffi::CString::new(p.as_os_str().as_encoded_bytes()) else {
        return false;
    };
    // SAFETY: c is a NUL-terminated path.
    unsafe { libc::access(c.as_ptr(), libc::X_OK) == 0 }
}

/// The server to start for a language and where its command is, or why
/// there is none (`{missing, install}`): the settings' choice first, then
/// Hangar's.
pub fn find(language: &str, settings: &Map<String, Value>) -> Result<(Spec, PathBuf), Value> {
    let mut specs = Vec::new();
    if let Some(own) = settings
        .get("languageServers")
        .and_then(|l| l.get(language))
        && let Some(command) = own
            .get("command")
            .and_then(Value::as_str)
            .filter(|c| !c.is_empty())
    {
        let args = own
            .get("args")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        specs.push(Spec {
            command: command.to_owned(),
            args,
            install: None,
        });
    }
    for k in KNOWN.iter().filter(|k| k.languages.contains(&language)) {
        specs.push(Spec {
            command: k.command.to_owned(),
            args: k.args.iter().map(|a| a.to_string()).collect(),
            install: k.install.map(str::to_owned),
        });
    }
    if specs.is_empty() {
        return Err(
            json!({ "missing": format!("Hangar knows no language server for {language}"), "install": [] }),
        );
    }
    for spec in &specs {
        let path = if spec.command.contains('/') {
            Some(PathBuf::from(&spec.command))
        } else {
            which(&spec.command)
        };
        if let Some(path) = path {
            return Ok((spec.clone(), path));
        }
    }
    let names: Vec<&str> = specs.iter().map(|s| s.command.as_str()).collect();
    Err(json!({
        "missing": format!("no language server for {language} is installed (looked for {})", names.join(", ")),
        "install": specs.iter().map(|s| {
            let mut m = json!({ "command": s.command });
            if let Some(i) = &s.install { m["install"] = json!(i); }
            m
        }).collect::<Vec<_>>(),
    }))
}

/// One language server process, speaking LSP's framing on its stdio.
pub struct Server {
    pub path: PathBuf,
    pub args: Vec<String>,
    pub pid: Option<u32>,
    pub started: f64,
    to: mpsc::UnboundedSender<Vec<u8>>,
    /// What it has written to stderr, its log, the latest 64 KiB.
    stderr: Arc<Mutex<String>>,
    kill: mpsc::UnboundedSender<Duration>,
}

impl Server {
    /// Starts the server in `root`; `message` is given each message it sends,
    /// and `exited` why it ended.
    pub fn start(
        root: &Path,
        spec: &Spec,
        path: &Path,
        message: impl Fn(Value) + Send + Sync + 'static,
        exited: impl FnOnce(String) + Send + 'static,
    ) -> std::io::Result<Server> {
        let mut c = child::command(&path.display().to_string(), &spec.args);
        c.current_dir(root)
            .env(
                "PATH",
                std::env::join_paths(search_path()).unwrap_or_default(),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut proc = c.spawn()?;
        let pid = proc.id();
        let mut stdin = proc.stdin.take().expect("piped stdin");
        let stdout = proc.stdout.take().expect("piped stdout");
        let mut stderr_pipe = proc.stderr.take().expect("piped stderr");
        let (to, mut outgoing) = mpsc::unbounded_channel::<Vec<u8>>();
        tokio::spawn(async move {
            while let Some(b) = outgoing.recv().await {
                if stdin.write_all(&b).await.is_err() {
                    return;
                }
            }
        });
        let stderr = Arc::new(Mutex::new(String::new()));
        let log = stderr.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 4096];
            while let Ok(n) = stderr_pipe.read(&mut buf).await {
                if n == 0 {
                    break;
                }
                let mut l = log.lock().unwrap();
                l.push_str(&String::from_utf8_lossy(&buf[..n]));
                let over = l.len().saturating_sub(64 << 10);
                if over > 0 {
                    let cut = (over..l.len())
                        .find(|&i| l.is_char_boundary(i))
                        .unwrap_or(l.len());
                    l.drain(..cut);
                }
            }
        });
        tokio::spawn(async move {
            let mut r = BufReader::new(stdout);
            loop {
                let mut len: Option<usize> = None;
                let mut line = String::new();
                loop {
                    line.clear();
                    match r.read_line(&mut line).await {
                        Ok(0) | Err(_) => return,
                        Ok(_) => {}
                    }
                    let l = line.trim_end();
                    if l.is_empty() {
                        break;
                    }
                    if let Some((k, v)) = l.split_once(':')
                        && k.trim().eq_ignore_ascii_case("content-length")
                    {
                        len = v.trim().parse().ok();
                    }
                }
                // Not LSP: what was read is dropped, in hope of a header.
                let Some(len) = len else { continue };
                let mut body = vec![0u8; len];
                if r.read_exact(&mut body).await.is_err() {
                    return;
                }
                if let Ok(m) = serde_json::from_slice::<Value>(&body) {
                    message(m);
                }
            }
        });
        let (kill, mut killed) = mpsc::unbounded_channel::<Duration>();
        let tail = stderr.clone();
        tokio::spawn(async move {
            let status = tokio::select! {
                s = proc.wait() => s,
                Some(after) = killed.recv() => {
                    tokio::select! {
                        s = proc.wait() => s,
                        _ = tokio::time::sleep(after) => {
                            let _ = proc.start_kill();
                            proc.wait().await
                        }
                    }
                }
            };
            let why = match status {
                Ok(s) => {
                    use std::os::unix::process::ExitStatusExt;
                    match (s.code(), s.signal()) {
                        (_, Some(sig)) => format!("stopped by signal {sig}"),
                        (Some(c), _) => format!("exited with {c}"),
                        _ => "exited".into(),
                    }
                }
                Err(e) => e.to_string(),
            };
            let t = tail.lock().unwrap().clone();
            let t: Vec<&str> = t.trim().lines().collect();
            let t = t[t.len().saturating_sub(3)..].join("\n");
            exited(if t.is_empty() {
                why
            } else {
                format!("{why}: {t}")
            });
        });
        Ok(Server {
            path: path.to_path_buf(),
            args: spec.args.clone(),
            pid,
            started: now_ms(),
            to,
            stderr,
            kill,
        })
    }

    /// Writes a message to the server.
    pub fn send(&self, mut m: Value) {
        if let Value::Object(o) = &mut m {
            let mut with = Map::new();
            with.insert("jsonrpc".into(), json!("2.0"));
            with.extend(std::mem::take(o));
            *o = with;
        }
        let body = serde_json::to_vec(&m).unwrap_or_default();
        let mut frame = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
        frame.extend(body);
        let _ = self.to.send(frame);
    }

    /// Tells the server of files changed on disk, which it may not be
    /// watching itself: a checkout, another tab's saves.
    pub fn files_changed(&self, paths: &[String]) {
        if paths.is_empty() {
            return;
        }
        let changes: Vec<Value> = paths
            .iter()
            .map(|p| json!({ "uri": file_url(p), "type": if Path::new(p).exists() { 2 } else { 3 } }))
            .collect();
        self.send(json!({ "method": "workspace/didChangeWatchedFiles", "params": { "changes": changes } }));
    }

    pub fn log(&self) -> String {
        self.stderr.lock().unwrap().clone()
    }

    /// Ends the server at once, for a service that is itself stopping.
    pub fn kill(&self) {
        let _ = self.kill.send(Duration::ZERO);
    }

    /// Asks the server to stop, as LSP has it, and then tells it.
    pub fn stop(&self) {
        self.send(json!({ "id": "hangar-shutdown", "method": "shutdown" }));
        self.send(json!({ "method": "exit" }));
        let _ = self.kill.send(Duration::from_secs(3));
    }
}

pub fn now_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64() * 1000.0)
        .unwrap_or(0.0)
}

/// A path as a file: URL, each segment escaped.
pub fn file_url(path: &str) -> String {
    let segs: Vec<String> = path
        .split('/')
        .map(|s| {
            let mut out = String::new();
            for b in s.bytes() {
                if b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b) {
                    out.push(b as char);
                } else {
                    out.push_str(&format!("%{b:02X}"));
                }
            }
            out
        })
        .collect();
    format!("file://{}", segs.join("/"))
}

/// A file: URL's path, its escapes undone.
pub fn url_path(uri: &str) -> Option<String> {
    let rest = uri.strip_prefix("file://")?;
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    let b = rest.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let Some(h) = b
                .get(i + 1..i + 3)
                .and_then(|h| std::str::from_utf8(h).ok())
            && let Ok(v) = u8::from_str_radix(h, 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8(out).ok()
}
