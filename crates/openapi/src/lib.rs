//! Hangar's HTTP APIs, generated from their OpenAPI specs by progenitor:
//! the client API (`/api/v1`, `api/client.yaml`), which the apps and tools
//! speak, and the web UI's (`/api/frontend`, `api/frontend.yaml`). Each
//! module holds its spec's types
//! (`types`) and an async client (`Client`); build.rs says what is changed
//! for the generator.

#[allow(warnings, clippy::all, clippy::pedantic)]
pub mod clientapi {
    include!(concat!(env!("OUT_DIR"), "/clientapi.rs"));
}

#[allow(warnings, clippy::all, clippy::pedantic)]
pub mod frontendapi {
    include!(concat!(env!("OUT_DIR"), "/frontendapi.rs"));
}

mod answers;

pub use answers::SignInStep;

/// A reqwest client builder for either API's `Client::new_with_client`:
/// its TLS is rustls on ring with the platform's verifier, as the rest of
/// Hangar's Rust connects, where reqwest's own would want aws-lc-rs (and
/// the generated `Client::new` panics for want of it). The timeouts and
/// headers, a token's `Authorization` among them, are the caller's.
pub fn http_client() -> Result<reqwest::ClientBuilder, rustls::Error> {
    use rustls_platform_verifier::ConfigVerifierExt;
    let tls = rustls::ClientConfig::with_platform_verifier()?;
    Ok(reqwest::Client::builder().tls_backend_preconfigured(tls))
}
