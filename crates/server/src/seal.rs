//! Secrets kept at rest, sealed with AES-256-GCM under the server's secret
//! key (`HANGAR_SECRET_KEY_FILE`): a version byte, a 12-byte nonce, then
//! the ciphertext and its tag. Each value is bound
//! to what it is -- the user and path it belongs to -- so a value copied
//! to another row does not open.

use ring::aead::{AES_256_GCM, Aad, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};

/// The first byte of every sealed value, for a change of scheme or key.
const VERSION: u8 = 1;

pub struct Sealer(LessSafeKey);

impl std::fmt::Debug for Sealer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Sealer")
    }
}

impl Sealer {
    /// Reads a 32-byte key, as hex or base64, from a file.
    pub fn load(file: &str) -> Result<Sealer, String> {
        let text = std::fs::read_to_string(file).map_err(|e| format!("{file}: {e}"))?;
        let text = text.trim();
        use base64::Engine;
        let key = hex(text)
            .or_else(|| base64::engine::general_purpose::STANDARD.decode(text).ok())
            .filter(|k| k.len() == 32)
            .ok_or_else(|| {
                format!("{file}: the secret key must be 32 bytes, as hex or base64 (openssl rand -hex 32)")
            })?;
        Ok(Sealer::new(&key))
    }

    fn new(key: &[u8]) -> Sealer {
        Sealer(LessSafeKey::new(
            UnboundKey::new(&AES_256_GCM, key).expect("a 32-byte key"),
        ))
    }

    /// Encrypts a secret, bound to what it is.
    pub fn seal(&self, plain: &[u8], bound: &str) -> Vec<u8> {
        let mut nonce = [0u8; NONCE_LEN];
        rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut nonce);
        let mut body = plain.to_vec();
        self.0
            .seal_in_place_append_tag(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(bound.as_bytes()),
                &mut body,
            )
            .expect("sealing");
        let mut out = Vec::with_capacity(1 + NONCE_LEN + body.len());
        out.push(VERSION);
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&body);
        out
    }

    /// Decrypts what seal made with the same binding.
    pub fn open(&self, sealed: &[u8], bound: &str) -> Result<Vec<u8>, String> {
        if sealed.len() < 1 + NONCE_LEN || sealed[0] != VERSION {
            return Err("a sealed value is malformed".into());
        }
        let nonce: [u8; NONCE_LEN] = sealed[1..1 + NONCE_LEN].try_into().expect("a nonce");
        let mut body = sealed[1 + NONCE_LEN..].to_vec();
        let plain = self
            .0
            .open_in_place(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(bound.as_bytes()),
                &mut body,
            )
            .map_err(|_| "a sealed value does not open with the server's key".to_string())?;
        Ok(plain.to_vec())
    }
}

fn hex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}
