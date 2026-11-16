//! A place's prefix names everything its worker makes on a Proxmox it may
//! share, so it must be valid in VM names, tags and storage file names.

use hangar_vm::proxmox::check_prefix;

#[test]
fn prefixes_proxmox_takes_everywhere() {
    for p in ["acme", "a", "hangar-dev", "team2-x", "abcdefghij0123456789"] {
        assert!(check_prefix(p).is_ok(), "{p} refused");
    }
}

#[test]
fn prefixes_some_name_would_refuse() {
    for p in [
        "",
        "Acme",
        "2acme",
        "-acme",
        "acme-",
        "ac_me",
        "ac.me",
        "ac me",
        "abcdefghij0123456789x",
    ] {
        assert!(check_prefix(p).is_err(), "{p:?} taken");
    }
}
