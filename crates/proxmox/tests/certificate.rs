//! A Hangar's HTTPS certificate is one a browser takes once a person
//! trusts it: Apple's platforms and Chrome want the address the browser
//! opens among its names, a TLS server's purpose, and at most 825 days.

use std::net::Ipv4Addr;

use hangar_proxmox::hangar::web_certificate;
use x509_parser::extensions::GeneralName;
use x509_parser::prelude::{FromDer, X509Certificate};

#[test]
fn a_hangars_certificate_is_one_browsers_take() {
    let address = Ipv4Addr::new(192, 168, 1, 178);
    let (der, key) = web_certificate(address).expect("a certificate");
    assert!(!key.is_empty());
    let (_, c) = X509Certificate::from_der(&der).expect("DER");

    let names = c
        .subject_alternative_name()
        .expect("its names")
        .expect("names at all");
    assert!(
        names
            .value
            .general_names
            .iter()
            .any(|n| matches!(n, GeneralName::IPAddress(ip) if *ip == [192, 168, 1, 178])),
        "the address is its name: {:?}",
        names.value.general_names
    );
    let usage = c
        .extended_key_usage()
        .expect("its purposes")
        .expect("purposes at all");
    assert!(usage.value.server_auth, "it is a TLS server's");
    let days = (c.validity().not_after.timestamp() - c.validity().not_before.timestamp()) / 86400;
    assert!(days <= 825, "it lasts {days} days");
    assert_eq!(c.subject(), c.issuer(), "it signs itself");

    let (again, _) = web_certificate(address).expect("another");
    let (_, again) = X509Certificate::from_der(&again).expect("DER");
    assert_ne!(
        c.raw_serial(),
        again.raw_serial(),
        "each has its own serial number, which a browser holds to its issuer's name"
    );
}
