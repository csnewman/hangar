//! TLS for the hub: the worker presents a certificate of its own, and the
//! agent takes it by its SHA-256 fingerprint alone, which the worker gave
//! it with the rest of its boot's settings. No authority signs it, and none
//! is asked: the fingerprint is the whole of the trust.

use std::io;
use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, WebPkiSupportedAlgorithms};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};

/// What both ends speak, so neither talks to anything else by mistake.
const ALPN: &[u8] = b"hangar-hub/1";

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// A certificate's fingerprint: SHA-256 of its DER, in lower-case hex.
pub fn fingerprint(cert: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, cert)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The agent's TLS: TLS 1.3 to the certificate with this fingerprint
/// alone.
pub fn client(fingerprint: &str) -> io::Result<Arc<rustls::ClientConfig>> {
    let fingerprint = fingerprint.trim().to_ascii_lowercase();
    if fingerprint.len() != 64 || !fingerprint.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a hub fingerprint is 64 hex digits",
        ));
    }
    let provider = provider();
    let pinned = Pinned {
        fingerprint,
        algorithms: provider.signature_verification_algorithms,
    };
    let mut c = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(io::Error::other)?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(pinned))
        .with_no_client_auth();
    c.alpn_protocols = vec![ALPN.to_vec()];
    Ok(Arc::new(c))
}

/// The worker's TLS: TLS 1.3, presenting cert (DER) and its PKCS #8 key.
pub fn server(cert: Vec<u8>, key: Vec<u8>) -> io::Result<Arc<rustls::ServerConfig>> {
    let mut c = rustls::ServerConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(io::Error::other)?
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from(cert)],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key)),
        )
        .map_err(io::Error::other)?;
    c.alpn_protocols = vec![ALPN.to_vec()];
    Ok(Arc::new(c))
}

/// A new certificate for a worker's hub, and its PKCS #8 key, both DER.
#[cfg(feature = "server")]
pub fn generate() -> io::Result<(Vec<u8>, Vec<u8>)> {
    let c = rcgen::generate_simple_self_signed(vec![crate::SERVER_NAME.to_string()])
        .map_err(io::Error::other)?;
    Ok((c.cert.der().to_vec(), c.signing_key.serialize_der()))
}

/// Takes the certificate with one fingerprint, and checks the handshake's
/// signatures were made with its key.
#[derive(Debug)]
struct Pinned {
    fingerprint: String,
    algorithms: WebPkiSupportedAlgorithms,
}

impl ServerCertVerifier for Pinned {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if fingerprint(end_entity) == self.fingerprint {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General(
                "the hub's certificate is not the one expected".into(),
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::General(
            "the hub speaks TLS 1.3 alone".into(),
        ))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}
