//! The Filesystem window's path field: a path typed as an absolute one,
//! and Tab completing a folder's name.

use hangar_app::project::{completion, parent_of, resolve};

#[test]
fn a_path_typed_is_read_from_home_root_or_the_folder_shown() {
    assert_eq!(resolve("~", "/etc", "/home/dev"), "/home/dev");
    assert_eq!(resolve("~/src/", "/etc", "/home/dev"), "/home/dev/src");
    assert_eq!(resolve("/var/log", "/etc", "/home/dev"), "/var/log");
    assert_eq!(resolve("nginx", "/etc", "/home/dev"), "/etc/nginx");
    assert_eq!(resolve("/", "/etc", "/home/dev"), "/");
    assert_eq!(parent_of("/etc/nginx"), "/etc");
    assert_eq!(parent_of("/etc"), "/");
    assert_eq!(parent_of("/"), "/");
}

#[test]
fn tab_completes_as_far_as_the_folders_agree() {
    let folders: Vec<String> = ["projects", "program-data", "public", ".profile.d"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(
        completion("/home/dev", "pro", &folders).as_deref(),
        Some("/home/dev/pro")
    );
    assert_eq!(
        completion("/home/dev", "proj", &folders).as_deref(),
        Some("/home/dev/projects/")
    );
    assert_eq!(completion("/", "pu", &folders).as_deref(), Some("/public/"));
    // A hidden folder only for a name typed with its dot.
    assert_eq!(
        completion("/home/dev", ".p", &folders).as_deref(),
        Some("/home/dev/.profile.d/")
    );
    assert_eq!(completion("/home/dev", "x", &folders), None);
}
