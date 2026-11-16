//! The guest's firewall, put in place in a network namespace of the test's
//! own: it is found only once there, the kernel takes every message of it,
//! again over itself, and loopback still works. It runs as root in a user namespace (`unshare`),
//! and is skipped where the system allows none.
#![cfg(target_os = "linux")]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::Command;

/// The exit status of a run that could not try: no namespace, or no
/// nf_tables in it.
const SKIPPED: i32 = 77;
const INSIDE: &str = "HANGAR_FIREWALL_TEST_INSIDE";

#[test]
fn installs_over_itself_and_keeps_loopback() {
    if std::env::var_os(INSIDE).is_none() {
        let exe = std::env::current_exe().unwrap();
        let status = Command::new("unshare")
            .args(["--user", "--map-root-user", "--net"])
            .arg(exe)
            .args([
                "--exact",
                "installs_over_itself_and_keeps_loopback",
                "--nocapture",
            ])
            .env(INSIDE, "1")
            .status();
        match status {
            Ok(s) if s.success() => {}
            Ok(s) if s.code() == Some(SKIPPED) => eprintln!("skipped: no nf_tables here"),
            Ok(s) if s.code() == Some(1) && !can_unshare() => {
                eprintln!("skipped: no user namespaces here")
            }
            Ok(s) => panic!("in its namespace, the test failed: {s}"),
            Err(e) => eprintln!("skipped: unshare: {e}"),
        }
        return;
    }

    match hangar_agent::firewall::installed() {
        Ok(there) => assert!(!there, "a new namespace has no firewall"),
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            eprintln!("nf_tables refused: {e}");
            std::process::exit(SKIPPED);
        }
        Err(e) => panic!("looking for the firewall: {e}"),
    }
    match hangar_agent::firewall::install() {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            eprintln!("nf_tables refused: {e}");
            std::process::exit(SKIPPED);
        }
        Err(e) => panic!("installing the firewall: {e}"),
    }
    assert!(hangar_agent::firewall::installed().unwrap());
    hangar_agent::firewall::install().expect("installing it again, over itself");
    assert!(hangar_agent::firewall::installed().unwrap());

    loopback_up();
    let ln = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = ln.local_addr().unwrap();
    let t = std::thread::spawn(move || {
        let (mut c, _) = ln.accept().unwrap();
        let mut b = [0u8; 5];
        c.read_exact(&mut b).unwrap();
        c.write_all(&b).unwrap();
    });
    let mut c = TcpStream::connect_timeout(&addr, std::time::Duration::from_secs(5))
        .expect("connecting over loopback");
    c.set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .unwrap();
    c.write_all(b"hello").unwrap();
    let mut b = [0u8; 5];
    c.read_exact(&mut b).unwrap();
    assert_eq!(&b, b"hello");
    t.join().unwrap();
}

fn can_unshare() -> bool {
    Command::new("unshare")
        .args(["--user", "--map-root-user", "--net", "true"])
        .status()
        .is_ok_and(|s| s.success())
}

/// A new network namespace's loopback starts down.
fn loopback_up() {
    // SAFETY: an ioctl socket, SIOCGIFFLAGS and SIOCSIFFLAGS on "lo".
    unsafe {
        let fd = libc::socket(libc::AF_INET, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0);
        assert!(fd >= 0);
        let mut req: libc::ifreq = std::mem::zeroed();
        for (i, b) in b"lo".iter().enumerate() {
            req.ifr_name[i] = *b as libc::c_char;
        }
        assert!(libc::ioctl(fd, libc::SIOCGIFFLAGS as _, &mut req) >= 0);
        req.ifr_ifru.ifru_flags |= libc::IFF_UP as libc::c_short;
        assert!(libc::ioctl(fd, libc::SIOCSIFFLAGS as _, &req) >= 0);
        libc::close(fd);
    }
}
