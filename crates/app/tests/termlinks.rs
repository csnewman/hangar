//! Where a link a terminal shows leads: a path from the shell's directory,
//! and an address on the machine itself to the environment's port.

use hangar_app::envtab::terminal_path;
use hangar_app::termview::local_port;

#[test]
fn paths_are_made_absolute() {
    let dir = Some("/home/dev/app");
    let home = Some("/home/dev");
    assert_eq!(
        terminal_path("src/main.rs", dir, home).as_deref(),
        Some("/home/dev/app/src/main.rs")
    );
    assert_eq!(
        terminal_path("./a/../b.rs", dir, home).as_deref(),
        Some("/home/dev/app/b.rs")
    );
    assert_eq!(
        terminal_path("../lib/x.go", dir, home).as_deref(),
        Some("/home/dev/lib/x.go")
    );
    assert_eq!(
        terminal_path("/etc/hosts.conf", None, None).as_deref(),
        Some("/etc/hosts.conf")
    );
    assert_eq!(
        terminal_path("~/notes.md", None, home).as_deref(),
        Some("/home/dev/notes.md")
    );
    assert_eq!(
        terminal_path("src/main.rs", None, home),
        None,
        "no directory"
    );
}

#[test]
fn local_addresses_are_the_environments_ports() {
    let port = |u: &str| local_port(u);
    assert_eq!(port("http://localhost:5173/"), Some((5173, "/".into())));
    assert_eq!(
        port("http://127.0.0.1:8080/a?b#c"),
        Some((8080, "/a?b#c".into()))
    );
    assert_eq!(port("http://0.0.0.0:3000"), Some((3000, "".into())));
    assert_eq!(port("http://[::1]:9000/x"), Some((9000, "/x".into())));
    assert_eq!(port("http://localhost/docs"), Some((80, "/docs".into())));
    assert_eq!(port("http://[::1]/"), Some((80, "/".into())));
    assert_eq!(port("https://localhost"), Some((443, "".into())));
    assert_eq!(port("https://example.com:8080/"), None);
    assert_eq!(port("ftp://localhost:21"), None);
    assert_eq!(port("http://localhost:x/"), None);
}
