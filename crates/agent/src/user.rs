//! The guest's users, and starting processes as one of them.

use std::ffi::CString;
use std::fs;
use std::io;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

/// The guest's SSH agent, which signs with the environment owner's keys
/// (internal/profile). Every process started for the user is given it.
pub const SSH_AUTH_SOCK: &str = "/run/hangar/ssh-agent.sock";

/// What loads Hangar's Claude Code plugin, written at boot, into every
/// claude started in a session (internal/agentstatus).
pub const PLUGIN_ENV: &str = "CLAUDE_CODE_PLUGIN_DIRS=/run/hangar/claude-plugin";

/// The environment's usual user, who a session belongs to unless it names
/// another.
pub const DEFAULT_USER: &str = "dev";

/// OOM scores (internal/oomscore). When memory runs out the kernel kills
/// the process with the highest badness: its share of memory in
/// thousandths plus its score, from -1000, never, to 1000. The agent runs at
/// -1000, since every session dies with it, and what it starts is given one
/// of these before it runs, so that whatever that starts inherits it too.
pub mod oom {
    /// The processes a browser's tabs run on, the Code service and the
    /// desktop's VNC server: small, and losing one closes every tab using
    /// it, so they are killed only when nothing else is left.
    pub const SERVICE: i32 = -900;
    /// Whatever the user runs, which the kernel ranks by size alone.
    pub const USER: i32 = 0;
}

/// A user, as /etc/passwd and /etc/group have them.
#[derive(Clone, Debug)]
pub struct User {
    pub name: String,
    pub uid: u32,
    pub gid: u32,
    pub home: PathBuf,
    /// The shell /etc/passwd gives, which may be empty or not exist.
    passwd_shell: String,
    /// Every group the user is in, their own first: without the
    /// supplementary ones a user in docker's group could not reach Docker.
    pub groups: Vec<u32>,
}

impl User {
    /// The user named `name`.
    pub fn lookup(name: &str) -> Option<User> {
        Self::find(|f| f[0] == name)
    }

    /// Whoever this process runs as. A user missing from /etc/passwd is
    /// their ID, at $HOME.
    pub fn current() -> User {
        // SAFETY: geteuid and getegid cannot fail.
        let (uid, gid) = unsafe { (libc::geteuid(), libc::getegid()) };
        let uid_s = uid.to_string();
        Self::find(|f| f[2] == uid_s).unwrap_or_else(|| User {
            name: uid_s.clone(),
            uid,
            gid,
            home: std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| "/".into()),
            passwd_shell: String::new(),
            groups: vec![gid],
        })
    }

    fn find(matches: impl Fn(&[&str]) -> bool) -> Option<User> {
        let passwd = fs::read_to_string("/etc/passwd").ok()?;
        let fields = passwd
            .lines()
            .map(|l| l.split(':').collect::<Vec<_>>())
            .find(|f| f.len() == 7 && matches(f))?;
        let (uid, gid) = (fields[2].parse().ok()?, fields[3].parse().ok()?);
        let name = fields[0].to_string();
        let mut groups = vec![gid];
        if let Ok(group) = fs::read_to_string("/etc/group") {
            for f in group.lines().map(|l| l.split(':').collect::<Vec<_>>()) {
                if f.len() == 4
                    && f[3].split(',').any(|m| m == name)
                    && let Ok(g) = f[2].parse()
                    && !groups.contains(&g)
                {
                    groups.push(g);
                }
            }
        }
        Some(User {
            name,
            uid,
            gid,
            home: PathBuf::from(fields[5]),
            passwd_shell: fields[6].to_string(),
            groups,
        })
    }

    /// The user's login shell: /etc/passwd's, unless it is missing or
    /// refuses logins, else bash, else sh.
    pub fn login_shell(&self) -> String {
        let s = &self.passwd_shell;
        if !s.is_empty() && Path::new(s).exists() && !s.ends_with("nologin") {
            return s.clone();
        }
        ["/bin/bash", "/bin/sh"]
            .into_iter()
            .find(|s| Path::new(s).exists())
            .unwrap_or("/bin/sh")
            .to_string()
    }
}

/// The environment every session the agent starts is given: the machine's
/// own (machine_env), and what loads Hangar's Claude Code plugin.
pub fn session_env() -> Vec<(String, String)> {
    let mut env = machine_env().to_vec();
    env.push(pair(PLUGIN_ENV));
    env
}

/// The environment the kernel's command line gives every process
/// (`systemd.setenv=NAME=value`), which systemd's services have. A session
/// the agent starts is given it too, so a program behaves the same started
/// from a terminal as from the desktop.
pub fn machine_env() -> &'static [(String, String)] {
    static ENV: OnceLock<Vec<(String, String)>> = OnceLock::new();
    ENV.get_or_init(|| {
        let cmdline = fs::read_to_string("/proc/cmdline").unwrap_or_default();
        cmdline
            .split_whitespace()
            .filter_map(|f| f.strip_prefix("systemd.setenv="))
            .filter(|kv| kv.contains('='))
            .map(pair)
            .collect()
    })
}

fn pair(kv: &str) -> (String, String) {
    let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
    (k.to_string(), v.to_string())
}

/// Makes `cmd` start at OOM score `oom`, and as `user`, with all their
/// groups, unless this process already is them. The score is set first,
/// while the child is still this process's user: a process that has changed
/// user is not dumpable, and its /proc files are root's.
pub fn prepare(cmd: &mut Command, user: Option<&User>, oom: i32) {
    let path = CString::new("/proc/self/oom_score_adj").unwrap();
    let score = oom.to_string().into_bytes();
    // SAFETY: geteuid cannot fail.
    let euid = unsafe { libc::geteuid() };
    let creds = user
        .filter(|u| u.uid != euid)
        .map(|u| (u.uid, u.gid, u.groups.clone()));
    // SAFETY: between fork and exec the closure calls only async-signal-safe
    // functions, on what was allocated before the fork.
    unsafe {
        cmd.pre_exec(move || {
            let fd = libc::open(path.as_ptr(), libc::O_WRONLY | libc::O_CLOEXEC);
            if fd >= 0 {
                // A score that cannot be set leaves the process ranked
                // wrongly, which is better than not starting it.
                libc::write(fd, score.as_ptr().cast(), score.len());
                libc::close(fd);
            }
            if let Some((uid, gid, groups)) = &creds
                && (libc::setgroups(groups.len() as _, groups.as_ptr().cast()) < 0
                    || libc::setgid(*gid) < 0
                    || libc::setuid(*uid) < 0)
            {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
}
