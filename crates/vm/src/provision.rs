//! Setting an environment up on its machine: its disks, the network and
//! profile waited for, the Code tab's trust, and its template applied the
//! first time it boots.

use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

use hangar_api::{CodeWorkspace, CodeWorkspaceFolder, EnvironmentSpec, Spec, Step, WORKSPACE_FILE};

use crate::agent::Session;
use crate::err;
use crate::gitout;
use crate::log::Log;

/// Records that an environment's template has been applied, so a restart
/// boots its disks as they are rather than cloning again.
pub const PROVISIONED_MARK: &str = "provisioned";

/// The guest's SSH agent, which signs with the owner's keys.
pub const SSH_AUTH_SOCK: &str = "/run/hangar/ssh-agent.sock";

/// There once the owner's profile has reached the guest.
pub const PROFILE_SYNCED: &str = "/run/hangar/profile-synced";

/// The folders the Code tab trusts to run their code, which the Code
/// service reads before starting a language server.
pub const TRUST_FILE: &str = "/run/hangar/trusted-folders.json";

/// Where the agent learns whether the environment hands back the memory its
/// programs leave idle (the spec's memory_reclaim): "1" while it does. It is
/// in /run, written at each boot and resume and whenever the setting
/// changes, so a running guest takes a change at once.
pub const RECLAIM_FILE: &str = "/run/hangar/memory-reclaim";

/// Where a clone's progress and errors are written in the guest, for the
/// host to read while it runs.
const CLONE_LOG: &str = "/run/hangar-clone.log";

/// How long a boot waits for the owner's profile.
const PROFILE_WAIT: Duration = Duration::from_secs(120);

/// Told what a start is doing.
pub trait Reporter: Send + Sync {
    /// The step it is on, and what it is doing in it.
    fn step(&self, step: Step, what: &str);
    /// How much of a step that can be measured is done.
    fn measure(&self, done: i64, total: i64, unit: &str);
}

/// The last line of the first text with any, or "no output".
pub fn first_line(texts: &[&str]) -> String {
    texts
        .iter()
        .map(|t| t.trim())
        .find(|t| !t.is_empty())
        .and_then(|t| t.lines().last())
        .map_or("no output".into(), |l| l.trim().to_string())
}

/// Creates an empty ext4 filesystem of gib GiB at path, unless one is
/// already there. The file is sparse: it takes space only as the guest
/// writes. mkfs is the program to make it with.
pub async fn ensure_disk(path: &Path, label: &str, gib: u64, mkfs: &Path) -> io::Result<()> {
    if path.exists() {
        return Ok(());
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".new");
    let tmp = std::path::PathBuf::from(tmp);
    std::fs::File::create(&tmp)?
        .set_len(gib << 30)
        .inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })?;
    let out = tokio::process::Command::new(mkfs)
        .args(["-q", "-F", "-L", label])
        .arg(&tmp)
        .output()
        .await;
    let failed = match &out {
        Ok(o) if o.status.success() => None,
        Ok(o) => Some(format!(
            "{}: {}",
            o.status,
            String::from_utf8_lossy(&[o.stdout.as_slice(), o.stderr.as_slice()].concat()).trim()
        )),
        Err(e) => Some(e.to_string()),
    };
    if let Some(why) = failed {
        let _ = std::fs::remove_file(&tmp);
        return Err(err(format!("making the {label} filesystem: {why}")));
    }
    // Renamed into place only once complete, so a disk interrupted half
    // made is made again rather than booted.
    std::fs::rename(&tmp, path)
}

/// mkfs.ext4: the host's, or on a worker on macOS, the one beside
/// hangar-krun.
pub fn find_mkfs() -> io::Result<std::path::PathBuf> {
    if let Some(p) = std::env::var_os("PATH")
        .iter()
        .flat_map(std::env::split_paths)
        .map(|d| d.join("mkfs.ext4"))
        .find(|p| p.is_file())
    {
        return Ok(p);
    }
    crate::krun::tool("mkfs.ext4")
        .map_err(|e| err(format!("no mkfs.ext4 to make filesystems with: {e}")))
}

/// Writes the folders the Code tab trusts to run their code, at each boot,
/// into /run, so it is always the environment's own and never outlives it.
pub async fn write_code_trust(sess: &Session, spec: &Spec) -> io::Result<()> {
    let b = serde_json::to_string(&spec.code_trust()).map_err(io::Error::other)?;
    let script = format!(
        "mkdir -p /run/hangar && printf '%s' \"$1\" > {TRUST_FILE}.new && chmod 644 {TRUST_FILE}.new && mv {TRUST_FILE}.new {TRUST_FILE}"
    );
    let out = sess
        .exec(Duration::from_secs(10), &["sh", "-c", &script, "sh", &b])
        .await?;
    if out.code != 0 {
        return Err(err(format!(
            "exit {}: {}",
            out.code,
            first_line(&[&out.stderr, &out.stdout])
        )));
    }
    Ok(())
}

/// Tells the guest's agent whether to reclaim the memory its programs leave
/// idle.
pub async fn write_reclaim(sess: &Session, on: bool) -> io::Result<()> {
    let script = format!(
        "mkdir -p /run/hangar && printf '%s' \"$1\" > {RECLAIM_FILE}.new && mv {RECLAIM_FILE}.new {RECLAIM_FILE}"
    );
    let v = if on { "1" } else { "0" };
    let out = sess
        .exec(Duration::from_secs(10), &["sh", "-c", &script, "sh", v])
        .await?;
    if out.code != 0 {
        return Err(err(format!(
            "exit {}: {}",
            out.code,
            first_line(&[&out.stderr, &out.stdout])
        )));
    }
    Ok(())
}

/// Waits for the guest's network and name resolver, on every boot: an
/// environment is running only once it can reach the outside, and its
/// workspace's clones need both. The agent answers early in the boot,
/// before either is up.
///
/// It waits for those targets alone, not for the whole machine: the rest
/// of the boot -- Docker, the desktop -- carries on starting in the guest
/// without holding the environment back.
pub async fn wait_network(sess: &Session, r: &dyn Reporter) -> io::Result<()> {
    r.step(Step::Network, "waiting for the network");
    // Starting a unit waits until it is up, and systemctl reaches systemd
    // without the system bus, which is not up this early.
    // nss-lookup.target cannot be started by hand, so the resolver is
    // waited for as itself, on the bases that have it.
    let out = sess
        .exec(
            Duration::from_secs(180),
            &[
                "sh",
                "-c",
                "systemctl start network-online.target &&\n\t\t{ ! systemctl cat systemd-resolved.service >/dev/null 2>&1 || systemctl start systemd-resolved.service; }",
            ],
        )
        .await
        .map_err(|e| err(format!("waiting for the network: {e}")))?;
    if out.code != 0 {
        return Err(err(format!(
            "the network did not come up: {}",
            first_line(&[&out.stderr, &out.stdout])
        )));
    }
    Ok(())
}

/// Waits for the owner's profile to reach the guest, so that the workspace
/// is set up, and the environment is running, with their settings and the
/// keys a clone over SSH signs in with. A profile that does not come holds
/// nothing else back: the environment starts without it, and a clone that
/// needs a key says so.
pub async fn wait_profile(sess: &Session, r: &dyn Reporter, log: &Log) {
    r.step(Step::Profile, "syncing the profile");
    let start = Instant::now();
    while start.elapsed() < PROFILE_WAIT {
        if let Ok(out) = sess
            .exec(Duration::from_secs(10), &["test", "-e", PROFILE_SYNCED])
            .await
            && out.code == 0
        {
            log.info(
                "the profile reached the guest",
                &[("after", &format!("{:?}", start.elapsed()))],
            );
            return;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    log.warn(
        "the profile did not reach the guest; starting without it",
        &[("waited", &format!("{PROFILE_WAIT:?}"))],
    );
}

/// The keys the owner's SSH agent in the guest offers, as a clone over SSH
/// sees them, to tell a key the git host does not know from no key at all.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentKeys {
    /// Fingerprint and type: "SHA256:... (ED25519)".
    pub keys: Vec<String>,
    /// Why there are none.
    pub why: String,
}

impl AgentKeys {
    /// The keys from ssh-add -l's exit code and output.
    pub fn parse(code: Option<i32>, out: &str) -> AgentKeys {
        match code {
            None | Some(2) => {
                return AgentKeys {
                    why: "the SSH agent could not be reached".into(),
                    ..Default::default()
                };
            }
            Some(1) => {
                return AgentKeys {
                    why: "the SSH agent offered no keys".into(),
                    ..Default::default()
                };
            }
            _ => {}
        }
        let keys = out
            .trim()
            .lines()
            .filter_map(|l| {
                // "256 SHA256:... comment (ED25519)"
                let f: Vec<&str> = l.split_whitespace().collect();
                (f.len() >= 2).then(|| format!("{} {}", f[1], f[f.len() - 1]))
            })
            .collect();
        AgentKeys {
            keys,
            why: String::new(),
        }
    }

    fn list(&self) -> String {
        if self.keys.is_empty() {
            return self.why.clone();
        }
        self.keys.join(", ")
    }

    /// Explains a git host's refusal of the keys.
    pub fn refused(&self) -> String {
        match self.keys.len() {
            0 => format!(
                "{}; add one under Account, then SSH keys (an environment whose template withholds SSH keys is given none).",
                self.why
            ),
            1 => format!(
                "the git host refused {}, the key the SSH agent offered; add it to your account there.",
                self.list()
            ),
            _ => format!(
                "the git host refused {}, the keys the SSH agent offered; add one to your account there.",
                self.list()
            ),
        }
    }
}

/// Whether git fetches url over SSH: an ssh:// URL, or the scp-like
/// user@host:path.
pub fn over_ssh(url: &str) -> bool {
    if let Some((scheme, _)) = url.split_once("://") {
        return scheme.starts_with("ssh") || scheme.ends_with("ssh");
    }
    url.split_once(':')
        .is_some_and(|(host, _)| !host.contains('/'))
}

fn strs(v: &[String]) -> Vec<&str> {
    v.iter().map(String::as_str).collect()
}

/// Runs a command in the guest, a failure being one that could not run or
/// exited non-zero.
async fn run(sess: &Session, timeout: Duration, what: &str, cmd: &[&str]) -> io::Result<()> {
    let out = sess
        .exec(timeout, cmd)
        .await
        .map_err(|e| err(format!("{what}: {e}")))?;
    if out.code != 0 {
        return Err(err(format!(
            "{what}: exit {}: {}",
            out.code,
            first_line(&[&out.stderr, &out.stdout])
        )));
    }
    Ok(())
}

/// Whether a command in the guest succeeds.
async fn done(sess: &Session, cmd: &[&str]) -> bool {
    sess.exec(Duration::from_secs(30), cmd)
        .await
        .is_ok_and(|o| o.code == 0)
}

/// Runs a clone, reporting git's progress as it goes. git writes its
/// progress to the log rather than to the reply, which comes only when the
/// command exits; the log is read once a second meanwhile. A failure is
/// explained by the line that names what went wrong, not by git's closing
/// advice.
async fn clone(sess: &Session, r: &dyn Reporter, url: &str, cmd: &[String]) -> io::Result<()> {
    let what = format!("cloning {url}");
    let mut full: Vec<&str> = vec![
        "sh",
        "-c",
        "log=$1; shift; \"$@\" 2>\"$log\"",
        "sh",
        CLONE_LOG,
    ];
    full.extend(cmd.iter().map(String::as_str));
    let cloning = sess.exec(Duration::from_secs(900), &full);
    tokio::pin!(cloning);
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.tick().await;
    let out = loop {
        tokio::select! {
            out = &mut cloning => break out,
            _ = tick.tick() => {
                if let Ok(o) = sess.exec(Duration::from_secs(5), &["tail", "-c", "512", CLONE_LOG]).await
                    && o.code == 0
                    && let Some((stage, d, t)) = gitout::progress(&o.stdout)
                {
                    r.step(Step::Workspace, &format!("{what}: {}", stage.to_lowercase()));
                    r.measure(d, t, "objects");
                }
            }
        }
    };
    let out = out.map_err(|e| err(format!("{what}: {e}")))?;
    if out.code != 0 {
        let why = match sess.exec(Duration::from_secs(5), &["cat", CLONE_LOG]).await {
            Ok(l) => gitout::failure(&l.stdout),
            Err(_) => "no output".into(),
        };
        return Err(err(format!("{what}: {why}")));
    }
    Ok(())
}

/// Applies an environment's template the first time it boots: its
/// hostname, its projects as the Code tab's workspace, and the repositories
/// it clones. Later boots find the mark in dir and leave the disks as they
/// are.
///
/// It is interrupted by a stop, a failure or the worker restarting, and runs
/// again from the top on the next boot -- the disks keep what it had done.
/// So each step is skipped if it is already done, rather than failing on
/// finding its own earlier work.
pub async fn provision(
    sess: &Session,
    dir: &Path,
    spec: &EnvironmentSpec,
    r: &dyn Reporter,
    log: &Log,
) -> io::Result<()> {
    let mark = dir.join(PROVISIONED_MARK);
    if mark.exists() {
        return Ok(());
    }
    let t30 = Duration::from_secs(30);
    let min = Duration::from_secs(60);
    r.step(Step::Workspace, "setting up the workspace");
    // Written directly rather than through hostnamectl: the agent is up
    // early in the boot, before the bus hostnamectl talks to. The name is a
    // DNS label, in lower case as host names are written, and reaches the
    // shell as an argument, never as script.
    let name = spec.name.to_lowercase();
    run(
        sess,
        t30,
        "setting the hostname",
        &[
            "sh",
            "-c",
            "printf '%s\\n' \"$1\" > /etc/hostname && hostname \"$1\"",
            "sh",
            &name,
        ],
    )
    .await?;

    // The workspace belongs to the person using it, so repositories are
    // cloned as dev where the image has that user. An image without one is
    // set up as root.
    let dev = sess
        .exec(Duration::from_secs(10), &["id", "-u", "dev"])
        .await
        .is_ok_and(|o| o.code == 0);
    let (as_user, owner): (&[&str], &str) = if dev {
        (&["runuser", "-u", "dev", "--"], "dev")
    } else {
        (&[], "root")
    };
    // runuser leaves the SSH agent out of git's environment, and a clone
    // over SSH signs in with the owner's keys through it.
    let ssh_env = format!("SSH_AUTH_SOCK={SSH_AUTH_SOCK}");
    let git = |args: &[&str]| -> Vec<String> {
        as_user
            .iter()
            .chain(&["env", ssh_env.as_str(), "git"])
            .chain(args)
            .map(|s| s.to_string())
            .collect()
    };

    // The projects, as the Code tab's workspace: written once, when there is
    // no workspace yet, the person changing it from then on. Its folder is
    // theirs, so the Code tab can write the workspace even when the spec
    // names no projects.
    let ws_dir = Path::new(WORKSPACE_FILE)
        .parent()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    run(
        sess,
        t30,
        &format!("creating {ws_dir}"),
        &["install", "-d", "-o", owner, "-g", owner, &ws_dir],
    )
    .await?;
    // An environment made before templates listed projects has its editor
    // folder and its repositories.
    let mut projects = spec.spec.projects.clone();
    if projects.is_empty() {
        for p in
            std::iter::once(&spec.spec.editor_path).chain(spec.spec.repos.iter().map(|r| &r.path))
        {
            if !p.is_empty() && !projects.contains(p) {
                projects.push(p.clone());
            }
        }
    }
    if !projects.is_empty() {
        let w = CodeWorkspace {
            folders: projects
                .iter()
                .map(|p| CodeWorkspaceFolder { path: p.clone() })
                .collect(),
        };
        let body = serde_json::to_string_pretty(&w).map_err(io::Error::other)?;
        let mut write: Vec<&str> = as_user.to_vec();
        write.extend([
            "sh",
            "-c",
            "[ -e \"$1\" ] || printf '%s\\n' \"$2\" > \"$1\"",
            "sh",
            WORKSPACE_FILE,
            &body,
        ]);
        run(sess, t30, "writing the workspace's projects", &write).await?;
    }

    // The keys are listed before the first clone over SSH, for the worker's
    // log, and named again if the git host refuses them.
    let mut keys: Option<AgentKeys> = None;
    for repo in &spec.spec.repos {
        r.step(Step::Workspace, &format!("cloning {}", repo.url));
        let parent = Path::new(&repo.path)
            .parent()
            .unwrap_or(Path::new("/"))
            .to_string_lossy()
            .into_owned();
        run(
            sess,
            min,
            &format!("creating {parent}"),
            &["install", "-d", "-o", owner, "-g", owner, &parent],
        )
        .await?;
        if !done(
            sess,
            &strs(&git(&["-C", &repo.path, "rev-parse", "--verify", "HEAD"])),
        )
        .await
        {
            // Cloned beside its destination and moved into place whole, so
            // an interrupted clone leaves nothing at the path for the next
            // attempt to trip over. mv -T refuses to replace a directory with
            // anything in it, so it never clobbers one it did not make.
            let partial = format!("{}.cloning", repo.path);
            run(
                sess,
                min,
                "clearing an earlier attempt",
                &["rm", "-rf", "--", &partial],
            )
            .await?;
            if keys.is_none() && over_ssh(&repo.url) {
                let mut cmd: Vec<&str> = as_user.to_vec();
                cmd.extend(["env", ssh_env.as_str(), "ssh-add", "-l"]);
                let k = match sess.exec(Duration::from_secs(45), &cmd).await {
                    Ok(o) => AgentKeys::parse(Some(o.code), &o.stdout),
                    Err(_) => AgentKeys::parse(None, ""),
                };
                log.info("the owner's SSH agent in the guest", &[("keys", &k.list())]);
                keys = Some(k);
            }
            // A git host refusing the keys is told which were offered.
            let refused = |e: io::Error| match &keys {
                Some(k) if e.to_string().contains("(publickey") => err(format!(
                    "{}: {}",
                    e.to_string().trim_end_matches('.'),
                    k.refused()
                )),
                _ => e,
            };
            clone(
                sess,
                r,
                &repo.url,
                &git(&["clone", "--progress", "--", &repo.url, &partial]),
            )
            .await
            .map_err(refused)?;
            if !repo.r#ref.is_empty() {
                run(
                    sess,
                    min,
                    &format!("checking out {}", repo.r#ref),
                    &strs(&git(&["-C", &partial, "checkout", &repo.r#ref])),
                )
                .await?;
            }
            // Its submodules, as the commit checked out names them, and
            // theirs: a repository is not all there without them.
            if done(sess, &["test", "-e", &format!("{partial}/.gitmodules")]).await {
                clone(
                    sess,
                    r,
                    &format!("{}'s submodules", repo.url),
                    &git(&[
                        "-C",
                        &partial,
                        "submodule",
                        "update",
                        "--init",
                        "--recursive",
                        "--progress",
                    ]),
                )
                .await
                .map_err(refused)?;
            }
            run(
                sess,
                min,
                &format!("moving the clone to {}", repo.path),
                &["mv", "-T", "--", &partial, &repo.path],
            )
            .await?;
        }
        if !repo.branch.is_empty()
            && !done(
                sess,
                &strs(&git(&[
                    "-C",
                    &repo.path,
                    "rev-parse",
                    "--verify",
                    &format!("refs/heads/{}", repo.branch),
                ])),
            )
            .await
        {
            run(
                sess,
                min,
                &format!("creating the branch {}", repo.branch),
                &strs(&git(&["-C", &repo.path, "switch", "-c", &repo.branch])),
            )
            .await?;
        }
    }
    std::fs::write(mark, b"")
}
