//! Whether this computer's browsers take a Hangar's own certificate, which
//! no authority signs: on macOS, trust settings in the user's login
//! keychain for TLS, set and removed by `security`, which asks for the
//! user's password itself. Elsewhere, none.

use std::io;

/// Whether this computer takes der as host's TLS certificate.
pub fn trusted(der: &[u8], host: &str) -> bool {
    #[cfg(target_os = "macos")]
    {
        let Ok(file) = pem_file(der) else {
            return false;
        };
        std::process::Command::new("/usr/bin/security")
            .args(["verify-cert", "-q", "-L", "-p", "ssl", "-s", host, "-c"])
            .arg(file.path())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (der, host);
        false
    }
}

/// Trusts der for TLS in the user's login keychain, macOS asking for the
/// user's password. It blocks until they answer.
pub fn trust(der: &[u8]) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        let file = pem_file(der)?;
        run(std::process::Command::new("/usr/bin/security")
            .args(["add-trusted-cert", "-r", "trustRoot", "-p", "ssl", "-k"])
            .arg(login_keychain()?)
            .arg(file.path()))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = der;
        Err(io::Error::other("trusting a certificate is macOS's alone"))
    }
}

/// Removes der's trust settings and der itself from the user's login
/// keychain, macOS asking for the user's password. It blocks until they
/// answer.
pub fn untrust(der: &[u8]) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        let file = pem_file(der)?;
        run(std::process::Command::new("/usr/bin/security")
            .arg("remove-trusted-cert")
            .arg(file.path()))?;
        let sha1: String = <sha1::Sha1 as sha1::Digest>::digest(der)
            .iter()
            .map(|b| format!("{b:02X}"))
            .collect();
        run(std::process::Command::new("/usr/bin/security")
            .args(["delete-certificate", "-Z", &sha1])
            .arg(login_keychain()?))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = der;
        Err(io::Error::other("trusting a certificate is macOS's alone"))
    }
}

#[cfg(target_os = "macos")]
fn login_keychain() -> io::Result<std::path::PathBuf> {
    let home = std::env::var_os("HOME").ok_or_else(|| io::Error::other("no home folder"))?;
    Ok(std::path::Path::new(&home).join("Library/Keychains/login.keychain-db"))
}

/// Runs a `security` command, its error output the error.
#[cfg(target_os = "macos")]
fn run(cmd: &mut std::process::Command) -> io::Result<()> {
    let out = cmd.output()?;
    if out.status.success() {
        return Ok(());
    }
    let why = String::from_utf8_lossy(&out.stderr).trim().to_string();
    Err(io::Error::other(if why.is_empty() {
        format!("security exited with {}", out.status)
    } else {
        why
    }))
}

/// der as a PEM file of its own, removed when dropped.
#[cfg(target_os = "macos")]
fn pem_file(der: &[u8]) -> io::Result<tempfile::NamedTempFile> {
    use base64::Engine;
    use std::io::Write;
    let b64 = base64::engine::general_purpose::STANDARD.encode(der);
    let mut f = tempfile::Builder::new().suffix(".pem").tempfile()?;
    writeln!(f, "-----BEGIN CERTIFICATE-----")?;
    for line in b64.as_bytes().chunks(64) {
        f.write_all(line)?;
        writeln!(f)?;
    }
    writeln!(f, "-----END CERTIFICATE-----")?;
    f.flush()?;
    Ok(f)
}
