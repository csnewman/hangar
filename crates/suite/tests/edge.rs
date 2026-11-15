//! What faces the internet directly: the zone's DNS server, HTTPS from a
//! certificate in files or one an ACME CA issues, and the redirect from
//! HTTP to HTTPS.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use hangar_suite::{HOST, Server, free_port};
use hickory_proto::op::{Edns, Message, MessageType, Query, ResponseCode};
use hickory_proto::rr::{Name, RData, RecordType};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn query(name: &str, t: RecordType, edns: bool) -> Message {
    let mut m = Message::query();
    m.metadata.recursion_desired = true;
    m.add_query(Query::query(Name::from_ascii(name).unwrap(), t));
    if edns {
        let mut e = Edns::new();
        e.set_max_payload(1232);
        m.set_edns(e);
    }
    m
}

/// Asks over UDP, again until the server answers: its DNS server starts
/// beside its HTTP one.
async fn udp(port: u16, q: &Message) -> Message {
    let sock = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let wire = q.to_vec().unwrap();
    for _ in 0..50 {
        sock.send_to(&wire, ("127.0.0.1", port)).await.unwrap();
        let mut buf = vec![0u8; 65535];
        match tokio::time::timeout(Duration::from_millis(200), sock.recv(&mut buf)).await {
            Ok(Ok(n)) => return Message::from_vec(&buf[..n]).expect("a DNS message"),
            _ => continue,
        }
    }
    panic!("no answer over UDP on {port}");
}

async fn tcp(port: u16, q: &Message) -> Message {
    let mut c = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("reaching DNS over TCP");
    let wire = q.to_vec().unwrap();
    c.write_u16(wire.len() as u16).await.unwrap();
    c.write_all(&wire).await.unwrap();
    let n = c.read_u16().await.unwrap() as usize;
    let mut buf = vec![0u8; n];
    c.read_exact(&mut buf).await.unwrap();
    Message::from_vec(&buf).expect("a DNS message")
}

fn addresses(m: &Message) -> Vec<String> {
    m.answers
        .iter()
        .filter_map(|r| match &r.data {
            RData::A(a) => Some(a.0.to_string()),
            RData::AAAA(a) => Some(a.0.to_string()),
            _ => None,
        })
        .collect()
}

fn soa_in_authority(m: &Message) -> bool {
    m.authorities
        .iter()
        .any(|r| matches!(r.data, RData::SOA(_)))
}

/// Every name in the zone points at the configured addresses, the apex
/// has its nameserver and SOA, a question with no answer carries the SOA,
/// and names outside the zone are refused, over UDP and TCP alike.
#[tokio::test]
async fn the_zone_answers_for_itself_alone() {
    let port = free_port();
    let Some(_s) = Server::start_with(&[
        ("HANGAR_DNS_LISTEN", format!("127.0.0.1:{port}")),
        ("HANGAR_DNS_ADDRESSES", "192.0.2.1, 2001:db8::1".into()),
    ]) else {
        return;
    };
    let apex = format!("{HOST}.");

    let a = udp(port, &query(&apex, RecordType::A, false)).await;
    assert_eq!(a.metadata.message_type, MessageType::Response);
    assert_eq!(a.metadata.response_code, ResponseCode::NoError);
    assert!(a.metadata.authoritative);
    assert!(!a.metadata.recursion_available);
    assert_eq!(addresses(&a), ["192.0.2.1"]);

    let env = format!("App-EnvAbcdef.{}.", HOST.to_uppercase());
    let aaaa = udp(port, &query(&env, RecordType::AAAA, false)).await;
    assert_eq!(addresses(&aaaa), ["2001:db8::1"]);
    assert_eq!(aaaa.answers[0].name.to_ascii(), env, "answered as asked");

    let any = tcp(
        port,
        &query(&format!("code.{HOST}."), RecordType::ANY, false),
    )
    .await;
    let mut got = addresses(&any);
    got.sort();
    assert_eq!(got, ["192.0.2.1", "2001:db8::1"]);

    let ns = udp(port, &query(&apex, RecordType::NS, false)).await;
    let names: Vec<String> = ns
        .answers
        .iter()
        .filter_map(|r| match &r.data {
            RData::NS(n) => Some(n.0.to_ascii()),
            _ => None,
        })
        .collect();
    assert_eq!(names, [format!("ns1.{HOST}.")]);

    let soa = udp(port, &query(&apex, RecordType::SOA, true)).await;
    let Some(RData::SOA(record)) = soa.answers.first().map(|r| &r.data) else {
        panic!("no SOA: {soa:?}");
    };
    assert_eq!(record.mname.to_ascii(), format!("ns1.{HOST}."));
    assert_eq!(record.rname.to_ascii(), format!("hostmaster.{HOST}."));
    assert_eq!(record.minimum, 60);
    assert!(soa.edns.is_some(), "EDNS is answered with EDNS");

    // A subdomain has no nameservers of its own: NODATA.
    let below = udp(port, &query(&format!("x.{HOST}."), RecordType::NS, false)).await;
    assert_eq!(below.metadata.response_code, ResponseCode::NoError);
    assert!(below.answers.is_empty());
    assert!(soa_in_authority(&below));

    let mx = udp(port, &query(&apex, RecordType::MX, false)).await;
    assert!(mx.answers.is_empty());
    assert!(soa_in_authority(&mx));

    let txt = udp(
        port,
        &query(&format!("_acme-challenge.{HOST}."), RecordType::TXT, false),
    )
    .await;
    assert_eq!(txt.metadata.response_code, ResponseCode::NoError);
    assert!(txt.answers.is_empty());
    assert!(soa_in_authority(&txt));

    let outside = udp(port, &query("example.com.", RecordType::A, false)).await;
    assert_eq!(outside.metadata.response_code, ResponseCode::Refused);
    assert!(!outside.metadata.authoritative);
    assert!(outside.answers.is_empty());
    let lookalike = tcp(port, &query(&format!("not{HOST}."), RecordType::A, false)).await;
    assert_eq!(lookalike.metadata.response_code, ResponseCode::Refused);
}

/// A scratch directory of the test's own.
fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("hangar-suite-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A CA, and a certificate it issued for the zone and every name under
/// it, written to dir: the certificate's chain and key files, and the CA.
fn issue(dir: &Path) -> (PathBuf, PathBuf, rustls::pki_types::CertificateDer<'static>) {
    let ca_key = rcgen::KeyPair::generate().unwrap();
    let mut ca = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
    ca.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    ca.distinguished_name
        .push(rcgen::DnType::CommonName, "Hangar suite CA");
    let ca_cert = ca.self_signed(&ca_key).unwrap();
    let issuer = rcgen::Issuer::new(ca, ca_key);
    let key = rcgen::KeyPair::generate().unwrap();
    let leaf = rcgen::CertificateParams::new(vec![HOST.to_string(), format!("*.{HOST}")])
        .unwrap()
        .signed_by(&key, &issuer)
        .unwrap();
    let (cert_file, key_file) = (dir.join("cert.pem"), dir.join("key.pem"));
    std::fs::write(&cert_file, leaf.pem() + &ca_cert.pem()).unwrap();
    std::fs::write(&key_file, key.serialize_pem()).unwrap();
    (cert_file, key_file, ca_cert.der().clone())
}

fn client(
    root: Option<&rustls::pki_types::CertificateDer<'static>>,
    alpn: &[&[u8]],
    names: &[&str],
    port: u16,
) -> (reqwest::Client, Arc<Mutex<Vec<Vec<u8>>>>) {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let builder = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .unwrap();
    let mut tls = match root {
        Some(root) => {
            let mut roots = rustls::RootCertStore::empty();
            roots.add(root.clone()).unwrap();
            builder.with_root_certificates(roots).with_no_client_auth()
        }
        None => builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(Anything {
                provider,
                seen: seen.clone(),
            }))
            .with_no_client_auth(),
    };
    tls.alpn_protocols = alpn.iter().map(|p| p.to_vec()).collect();
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let mut b = reqwest::Client::builder()
        .tls_backend_preconfigured(tls)
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(10));
    for n in names {
        b = b.resolve(n, addr);
    }
    (b.build().unwrap(), seen)
}

/// Trusts any certificate, keeping each leaf it is shown: for a CA whose
/// root the test does not have.
#[derive(Debug)]
struct Anything {
    provider: Arc<rustls::crypto::CryptoProvider>,
    seen: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl rustls::client::danger::ServerCertVerifier for Anything {
    fn verify_server_cert(
        &self,
        end_entity: &rustls::pki_types::CertificateDer<'_>,
        _: &[rustls::pki_types::CertificateDer<'_>],
        _: &rustls::pki_types::ServerName<'_>,
        _: &[u8],
        _: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        self.seen.lock().unwrap().push(end_entity.to_vec());
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// GETs url until the server answers it: HTTPS starts beside HTTP.
async fn eventually_get(c: &reqwest::Client, url: &str) -> reqwest::Response {
    for _ in 0..100 {
        if let Ok(r) = c.get(url).send().await {
            return r;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    c.get(url)
        .send()
        .await
        .unwrap_or_else(|e| panic!("GET {url}: {e:?}"))
}

/// HTTPS serves the certificate in the files, over HTTP/2 or HTTP/1.1 as
/// the client asks, to Hangar's host and its environments' alike; a
/// session's cookie set over it is Secure; and the redirect sends plain
/// HTTP to it, except for an environment's hosts, which it serves.
#[tokio::test]
async fn https_serves_the_certificate_in_files() {
    let dir = scratch("tls-files");
    let (cert, key, root) = issue(&dir);
    let (tls, redirect) = (free_port(), free_port());
    let Some(_s) = Server::start_with(&[
        ("HANGAR_PUBLIC_URL", format!("https://{HOST}:{tls}")),
        ("HANGAR_TLS_LISTEN", format!("127.0.0.1:{tls}")),
        ("HANGAR_TLS_CERT_FILE", cert.display().to_string()),
        ("HANGAR_TLS_KEY_FILE", key.display().to_string()),
        ("HANGAR_REDIRECT_LISTEN", format!("127.0.0.1:{redirect}")),
    ]) else {
        return;
    };
    let env = format!("app-envabcdef.{HOST}");

    let (h2, _) = client(Some(&root), &[b"h2", b"http/1.1"], &[HOST, &env], tls);
    let r = eventually_get(&h2, &format!("https://{HOST}:{tls}/api/healthz")).await;
    assert_eq!(r.status(), 200);
    assert_eq!(r.version(), reqwest::Version::HTTP_2);
    let r = h2
        .get(format!("https://{env}:{tls}/"))
        .send()
        .await
        .expect("the wildcard covers an environment's host");
    assert_eq!(r.status(), 404, "there is no such environment");

    let (h1, _) = client(Some(&root), &[b"http/1.1"], &[HOST], tls);
    let r = h1
        .post(format!("https://{HOST}:{tls}/api/frontend/auth/login"))
        .header("Origin", format!("https://{HOST}:{tls}"))
        .json(&serde_json::json!({
            "username": hangar_suite::ADMIN,
            "password": hangar_suite::ADMIN_PASSWORD,
        }))
        .send()
        .await
        .expect("signing in over HTTPS");
    assert_eq!(r.version(), reqwest::Version::HTTP_11);
    assert!(r.status().is_success(), "{}", r.status());
    let cookie = r
        .headers()
        .get("set-cookie")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(cookie.contains("Secure"), "{cookie}");

    let addr = SocketAddr::from(([127, 0, 0, 1], redirect));
    let plain = hangar_openapi::http_client()
        .unwrap()
        .redirect(reqwest::redirect::Policy::none())
        .resolve(HOST, addr)
        .resolve(&env, addr)
        .build()
        .unwrap();
    let r = eventually_get(&plain, &format!("http://{HOST}:{redirect}/some/where?x=1")).await;
    assert_eq!(r.status(), 308);
    assert_eq!(
        r.headers().get("location").and_then(|v| v.to_str().ok()),
        Some(format!("https://{HOST}:{tls}/some/where?x=1").as_str())
    );
    // Plain HTTP reaches an environment on the public port's host names.
    let r = plain
        .get(format!("http://{env}:{redirect}/"))
        .header("Host", format!("{env}:{tls}"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        r.status(),
        404,
        "an environment's host is served, not redirected"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// A certificate's names, from its DER.
fn names(der: &[u8]) -> Vec<String> {
    let (_, c) = x509_parser::parse_x509_certificate(der).unwrap();
    c.subject_alternative_name()
        .ok()
        .flatten()
        .map(|s| {
            s.value
                .general_names
                .iter()
                .filter_map(|n| match n {
                    x509_parser::extensions::GeneralName::DNSName(d) => Some(d.to_string()),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The zone's certificates are issued by a real ACME CA, the zone's own
/// DNS answering its DNS-01 challenges: one for the zone, and a wildcard
/// one for every name under it. It runs against Pebble, Let's Encrypt's
/// test CA, when these are set:
///
///   HANGAR_SUITE_ACME_DIRECTORY   Pebble's directory, https://localhost:14000/dir
///   HANGAR_SUITE_ACME_ROOTS       the PEM root Pebble's own endpoint is served under
///   HANGAR_SUITE_ACME_DNS_LISTEN  where to serve the zone: Pebble's -dnsserver
#[tokio::test]
async fn acme_issues_the_zones_certificates() {
    let Ok(directory) = std::env::var("HANGAR_SUITE_ACME_DIRECTORY") else {
        return;
    };
    let roots = std::env::var("HANGAR_SUITE_ACME_ROOTS").expect("HANGAR_SUITE_ACME_ROOTS");
    let dns = std::env::var("HANGAR_SUITE_ACME_DNS_LISTEN").expect("HANGAR_SUITE_ACME_DNS_LISTEN");
    let dns_port: u16 = dns.rsplit(':').next().unwrap().parse().unwrap();
    let tls = free_port();
    let Some(_s) = Server::start_with(&[
        ("HANGAR_PUBLIC_URL", format!("https://{HOST}:{tls}")),
        ("HANGAR_TLS_LISTEN", format!("127.0.0.1:{tls}")),
        ("HANGAR_DNS_LISTEN", dns),
        ("HANGAR_DNS_ADDRESSES", "127.0.0.1".into()),
        ("HANGAR_ACME_CA", directory),
        ("HANGAR_ACME_CA_ROOTS", roots),
        ("HANGAR_ACME_EMAIL", format!("admin@{HOST}")),
    ]) else {
        return;
    };
    let env = format!("app-envabcdef.{HOST}");
    let served = async |host: &str| {
        let (c, seen) = client(None, &[b"http/1.1"], &[host], tls);
        for _ in 0..240 {
            if c.get(format!("https://{host}:{tls}/api/healthz"))
                .send()
                .await
                .is_ok()
            {
                return names(seen.lock().unwrap().last().unwrap());
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        panic!("no certificate was served for {host}");
    };
    assert_eq!(served(HOST).await, [HOST.to_string()]);
    assert_eq!(served(&env).await, [format!("*.{HOST}")]);

    // The challenges' records are gone once they are done.
    let txt = udp(
        dns_port,
        &query(&format!("_acme-challenge.{HOST}."), RecordType::TXT, false),
    )
    .await;
    assert!(txt.answers.is_empty(), "challenge records left: {txt:?}");
}
