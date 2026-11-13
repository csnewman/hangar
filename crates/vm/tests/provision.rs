//! Provisioning, against a scripted guest that records what it is asked
//! to run; and what git and ssh-add say, read as internal/gitout and
//! internal/vm read them.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use hangar_api::{EnvironmentSpec, Repo, Step};
use hangar_vm::gitout;
use hangar_vm::provision::{self, AgentKeys, Reporter};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[test]
fn git_progress() {
    let out = "Cloning into 'x.cloning'...\nremote: Enumerating objects: 2742, done.\n\
               Receiving objects:  12% (330/2742)\rReceiving objects:  45% (1234/2742), 1.20 MiB | 2.00 MiB/s\r";
    assert_eq!(
        gitout::progress(out),
        Some(("Receiving objects".into(), 1234, 2742))
    );
    let out = format!(
        "{out}Receiving objects: 100% (2742/2742), done.\nResolving deltas:  10% (80/800)\r"
    );
    assert_eq!(
        gitout::progress(&out),
        Some(("Resolving deltas".into(), 80, 800))
    );
    assert_eq!(gitout::progress("Cloning into 'x'...\n"), None);
}

#[test]
fn git_failure() {
    for (out, want) in [
        (
            "Cloning into '/workspace/app.cloning'...\n\
             Warning: Permanently added 'github.com' (ED25519) to the list of known hosts.\n\
             git@github.com: Permission denied (publickey).\n\
             fatal: Could not read from remote repository.\n\n\
             Please make sure you have the correct access rights\nand the repository exists.\n",
            "git@github.com: Permission denied (publickey).",
        ),
        (
            "Cloning into 'x'...\nHost key verification failed.\nfatal: Could not read from remote repository.\n\n\
             Please make sure you have the correct access rights\nand the repository exists.\n",
            "Host key verification failed.",
        ),
        (
            "Cloning into 'x'...\nremote: Repository not found.\nfatal: repository 'https://github.com/a/b.git/' not found\n",
            "fatal: repository 'https://github.com/a/b.git/' not found",
        ),
        ("", "no output"),
    ] {
        assert_eq!(gitout::failure(out), want);
    }
}

#[test]
fn ssh_urls_and_keys() {
    for (url, ssh) in [
        ("git@github.com:a/b.git", true),
        ("ssh://git@host/a", true),
        ("git+ssh://host/a", true),
        ("https://github.com/a/b", false),
        ("/srv/repo", false),
        ("./a:b", false),
    ] {
        assert_eq!(provision::over_ssh(url), ssh, "{url}");
    }
    let k = AgentKeys::parse(
        Some(0),
        "256 SHA256:abc me@laptop (ED25519)\n3072 SHA256:def work (RSA)\n",
    );
    assert_eq!(k.keys, ["SHA256:abc (ED25519)", "SHA256:def (RSA)"]);
    assert!(
        k.refused()
            .starts_with("the git host refused SHA256:abc (ED25519), SHA256:def (RSA), the keys")
    );
    let none = AgentKeys::parse(Some(1), "The agent has no identities.\n");
    assert!(
        none.refused()
            .starts_with("the SSH agent offered no keys; add one under Account")
    );
    assert_eq!(
        AgentKeys::parse(None, "").why,
        "the SSH agent could not be reached"
    );
    assert_eq!(provision::first_line(&["", "  a\nb  \n"]), "b");
    assert_eq!(provision::first_line(&["", " "]), "no output");
}

// A disk is made once, by mkfs, and renamed into place only when made.
#[tokio::test]
async fn ensure_disk() {
    let d = std::env::temp_dir().join(format!("hangar-vm-{}-disk", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let mkfs = d.join("mkfs");
    std::fs::write(
        &mkfs,
        "#!/bin/sh\necho \"$@\" > \"$(dirname \"$0\")/args\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&mkfs, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let disk = d.join("upper.ext4");
    provision::ensure_disk(&disk, "hangar-upper", 2, &mkfs)
        .await
        .unwrap();
    assert_eq!(std::fs::metadata(&disk).unwrap().len(), 2 << 30);
    let args = std::fs::read_to_string(d.join("args")).unwrap();
    assert_eq!(
        args.trim(),
        format!("-q -F -L hangar-upper {}.new", disk.display())
    );
    std::fs::remove_file(d.join("args")).unwrap();
    provision::ensure_disk(&disk, "hangar-upper", 2, &mkfs)
        .await
        .unwrap();
    assert!(
        !d.join("args").exists(),
        "a disk that was there was made again"
    );

    std::fs::write(&mkfs, "#!/bin/sh\necho 'no space' >&2\nexit 1\n").unwrap();
    let e = provision::ensure_disk(&d.join("other.ext4"), "x", 1, &mkfs)
        .await
        .unwrap_err();
    assert!(e.to_string().contains("no space"), "{e}");
    assert!(!d.join("other.ext4").exists() && !d.join("other.ext4.new").exists());
    let _ = std::fs::remove_dir_all(d);
}

/// The guest's answers: a command is run as it is matched, the first pattern
/// it contains giving the exit code; anything else exits 0.
type Script = Vec<(&'static str, i32, &'static str)>;

async fn scripted_guest(socket: PathBuf, script: Script, asked: Arc<Mutex<Vec<Vec<String>>>>) {
    let c = loop {
        match tokio::net::UnixStream::connect(&socket).await {
            Ok(c) => break c,
            Err(_) => tokio::time::sleep(Duration::from_millis(10)).await,
        }
    };
    let (r, mut w) = c.into_split();
    w.write_all(b"{\"kind\":\"hello\",\"version\":1}\n")
        .await
        .unwrap();
    let mut lines = BufReader::new(r).lines();
    while let Ok(Some(l)) = lines.next_line().await {
        let req: serde_json::Value = serde_json::from_str(&l).unwrap();
        let cmd: Vec<String> = req["cmd"]
            .as_array()
            .map(|a| a.iter().map(|v| v.as_str().unwrap().to_string()).collect())
            .unwrap_or_default();
        let line = cmd.join(" ");
        let (code, out) = script
            .iter()
            .find(|(p, _, _)| line.contains(p))
            .map_or((0, ""), |(_, c, o)| (*c, *o));
        asked.lock().unwrap().push(cmd);
        let reply = serde_json::json!({"id": req["id"], "code": code, "stdout": out});
        w.write_all(format!("{reply}\n").as_bytes()).await.unwrap();
    }
}

#[derive(Default)]
struct Steps(Mutex<Vec<(Step, String)>>);

impl Reporter for Steps {
    fn step(&self, step: Step, what: &str) {
        self.0.lock().unwrap().push((step, what.to_string()));
    }
    fn measure(&self, _: i64, _: i64, _: &str) {}
}

#[tokio::test]
async fn provision() {
    let d = PathBuf::from(format!("/tmp/hvm-{}-prov", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let srv = hangar_vm::agent::Server::listen_hybrid(&d.join("v"), 3).unwrap();
    let asked = Arc::new(Mutex::new(vec![]));
    let script: Script = vec![
        ("rev-parse --verify HEAD", 1, ""),
        ("rev-parse --verify refs/heads/", 1, ""),
        ("/.gitmodules", 1, ""),
        ("ssh-add -l", 0, "256 SHA256:abc me (ED25519)\n"),
    ];
    tokio::spawn(scripted_guest(d.join("v_8100"), script, asked.clone()));
    let sess = srv.accept(Some(Duration::from_secs(5))).await.unwrap();

    let mut spec: EnvironmentSpec =
        serde_json::from_str(r#"{"id":"e","name":"Ticket-42","desired":"running"}"#).unwrap();
    spec.spec.repos = vec![Repo {
        url: "git@github.com:a/b.git".into(),
        r#ref: "main".into(),
        path: "/home/dev/b".into(),
        branch: "ticket-42".into(),
    }];
    spec.spec.editor_path = "/home/dev/b".into();
    let steps = Steps::default();
    let log = hangar_vm::log::Log::default();
    provision::provision(&sess, &d, &spec, &steps, &log)
        .await
        .unwrap();
    assert!(d.join(provision::PROVISIONED_MARK).exists());

    let asked: Vec<String> = asked.lock().unwrap().iter().map(|c| c.join(" ")).collect();
    let git = "runuser -u dev -- env SSH_AUTH_SOCK=/run/hangar/ssh-agent.sock git";
    let want = [
        "sh -c printf '%s\\n' \"$1\" > /etc/hostname && hostname \"$1\" sh ticket-42".to_string(),
        "id -u dev".into(),
        "install -d -o dev -g dev /var/lib/hangar-workspace".into(),
        "runuser -u dev -- sh -c [ -e \"$1\" ] || printf '%s\\n' \"$2\" > \"$1\" sh /var/lib/hangar-workspace/projects.code-workspace {\n  \"folders\": [\n    {\n      \"path\": \"/home/dev/b\"\n    }\n  ]\n}".into(),
        "install -d -o dev -g dev /home/dev".into(),
        format!("{git} -C /home/dev/b rev-parse --verify HEAD"),
        "rm -rf -- /home/dev/b.cloning".into(),
        "runuser -u dev -- env SSH_AUTH_SOCK=/run/hangar/ssh-agent.sock ssh-add -l".into(),
        format!("sh -c log=$1; shift; \"$@\" 2>\"$log\" sh /run/hangar-clone.log {git} clone --progress -- git@github.com:a/b.git /home/dev/b.cloning"),
        format!("{git} -C /home/dev/b.cloning checkout main"),
        "test -e /home/dev/b.cloning/.gitmodules".into(),
        "mv -T -- /home/dev/b.cloning /home/dev/b".into(),
        format!("{git} -C /home/dev/b rev-parse --verify refs/heads/ticket-42"),
        format!("{git} -C /home/dev/b switch -c ticket-42"),
    ];
    assert_eq!(asked, want);
    assert_eq!(
        steps.0.lock().unwrap()[1],
        (Step::Workspace, "cloning git@github.com:a/b.git".into())
    );

    // Provisioned once: a later boot asks nothing.
    let before = want.len();
    provision::provision(&sess, &d, &spec, &steps, &log)
        .await
        .unwrap();
    assert_eq!(before, want.len());
    let _ = std::fs::remove_dir_all(d);
}
