//! The process list, on Linux, whose /proc it reads.
#![cfg(target_os = "linux")]

use std::process::Command;
use std::time::Duration;

use hangar_agent::procs::{Process, Request, Server};

fn find(ps: &[Process], pid: u32) -> Option<&Process> {
    ps.iter().find(|p| p.pid == pid as i64)
}

fn ask(srv: &Server, op: &str, pid: u32, signal: &str) -> hangar_agent::procs::Reply {
    srv.handle(&Request {
        op: op.into(),
        pid: pid as i64,
        signal: signal.into(),
    })
}

#[test]
fn list_and_stop() {
    // One process busy on a CPU, and one asleep.
    let mut busy = Command::new("sh")
        .args(["-c", "while :; do :; done"])
        .spawn()
        .unwrap();
    let mut idle = Command::new("sleep").arg("60").spawn().unwrap();
    let srv = Server::new();

    let first = ask(&srv, "list", 0, "");
    assert!(first.error.is_empty() && first.cpus >= 1 && first.memory_bytes > 0);
    let me = find(&first.processes, std::process::id()).expect("this process");
    assert!(me.protected, "the serving process is not marked protected");
    let p = find(&first.processes, idle.id()).expect("the sleeping process");
    assert_eq!((p.command.as_str(), p.name.as_str()), ("sleep 60", "sleep"));
    assert_eq!(p.ppid, std::process::id() as i64);

    std::thread::sleep(Duration::from_secs(1));
    let second = ask(&srv, "list", 0, "");
    let b = find(&second.processes, busy.id()).expect("the busy process");
    assert!(b.cpu_percent >= 10.0, "the busy process: {b:?}");
    let s = find(&second.processes, idle.id()).expect("the sleeping process");
    assert!(s.cpu_percent <= 5.0, "the sleeping process: {s:?}");

    assert_eq!(ask(&srv, "kill", idle.id(), "").error, "");
    assert!(idle.wait().is_ok());
    assert!(
        !ask(&srv, "kill", 1, "").error.is_empty(),
        "init was stopped"
    );
    assert!(
        !ask(&srv, "kill", busy.id(), "HUP").error.is_empty(),
        "an unknown signal was sent"
    );
    let _ = busy.kill();
    let _ = busy.wait();
}
