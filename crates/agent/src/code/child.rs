//! The agent starts this service protected from the kernel's OOM killer
//! (internal/oomscore), and what the service starts would inherit that: a
//! language server of several gigabytes, or a commit hook's build, would
//! outlive the user's own work when memory runs out. So it starts them
//! through a shell that first gives them the score of anything else the user
//! runs, and then becomes them.

use std::ffi::OsStr;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::process::Command;

const UNPROTECT: &str = "{ echo 0 >/proc/self/oom_score_adj; } 2>/dev/null; exec \"$0\" \"$@\"";

/// A command for `file` and `args`, started as above.
pub fn command<S: AsRef<OsStr>>(file: &str, args: &[S]) -> Command {
    let mut c = if cfg!(target_os = "linux") {
        let mut c = Command::new("/bin/sh");
        c.arg("-c").arg(UNPROTECT).arg(file);
        c
    } else {
        Command::new(file)
    };
    c.args(args);
    c.kill_on_drop(true);
    c
}

/// What a command printed, and how it ended.
pub struct Output {
    pub status: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub timed_out: bool,
}

/// Runs `c` to the end, writing `input` to it first, killed after `timeout`.
pub async fn run(
    mut c: Command,
    input: Option<&[u8]>,
    timeout: Option<Duration>,
) -> std::io::Result<Output> {
    c.stdin(if input.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    })
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    let mut child = c.spawn()?;
    if let Some(input) = input {
        let mut stdin = child.stdin.take().expect("piped stdin");
        let input = input.to_vec();
        // Written alongside reading, so a large input can't wait on a full
        // output pipe.
        tokio::spawn(async move {
            let _ = stdin.write_all(&input).await;
        });
    }
    let wait = child.wait_with_output();
    let out = match timeout {
        Some(t) => match tokio::time::timeout(t, wait).await {
            Ok(out) => out?,
            Err(_) => {
                return Ok(Output {
                    status: None,
                    stdout: Vec::new(),
                    stderr: Vec::new(),
                    timed_out: true,
                });
            }
        },
        None => wait.await?,
    };
    Ok(Output {
        status: out.status.code(),
        stdout: out.stdout,
        stderr: out.stderr,
        timed_out: false,
    })
}
