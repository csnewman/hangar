//! How a Hangar on a Proxmox names what it makes, which places sharing a
//! Proxmox rely on to leave each other's things alone.

use hangar_proxmox::hangar::{check_prefix, is_own_iso, prefix_of, release_key, show_fingerprint};

#[test]
fn a_prefix_made_of_a_name_is_one_proxmox_takes() {
    for (name, want) in [
        ("Homelab", "homelab"),
        ("Chandler's box", "chandler-s-box"),
        ("  Team 2 / dev  ", "team-2-dev"),
        ("2nd floor", "h2nd-floor"),
        ("Image builder", "builder"),
        (
            "A very long name for a Hangar indeed",
            "a-very-long-name-for",
        ),
    ] {
        let p = prefix_of(name);
        assert_eq!(p, want, "{name:?}");
        assert!(check_prefix(&p).is_ok(), "{p:?} refused");
    }
}

#[test]
fn a_hangar_takes_only_its_own_isos() {
    assert!(is_own_iso("acme", "acme-release-0123456789ab.iso"));
    assert!(is_own_iso("acme", "acme-image-0123456789ab.iso"));
    assert!(!is_own_iso("acme", "acme-dev-image-0123456789ab.iso"));
    assert!(!is_own_iso("acme", "acmex-image-0123456789ab.iso"));
    assert!(!is_own_iso("acme", "acme-image-0123456789ab.img"));
    assert!(!is_own_iso("acme", "ubuntu-26.04.iso"));
}

#[test]
fn a_release_is_named_by_its_version() {
    assert_eq!(
        release_key("8106f14919900108b8a2d4c0d9a7bda9c357dbb4"),
        "8106f1491990"
    );
    assert_eq!(release_key("solo-1"), "solo-1");
    let odd = release_key("Release 1/2");
    assert_eq!(odd.len(), 12);
    assert!(odd.bytes().all(|b| b.is_ascii_hexdigit()));
}

#[test]
fn fingerprints_show_as_proxmox_shows_them() {
    assert_eq!(show_fingerprint("3a7f0c"), "3A:7F:0C");
    assert_eq!(show_fingerprint("3A:7F:0C"), "3A:7F:0C");
}
