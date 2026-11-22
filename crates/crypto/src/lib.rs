//! # veloxdesk-crypto
//!
//! Device identity (Ed25519), TLS 1.3 certificates bound to that identity, and the
//! rustls configurations used by the QUIC transport (ARCHITECTURE ADR-003, §7).
//!
//! * [`DeviceIdentity`] — Ed25519 keypair, 9-digit device id, persisted with
//!   [`DeviceIdentity::load_or_create`].
//! * [`tls`] — self-signed certificate signed by the device key, and client/server
//!   rustls configs whose custom verifiers authenticate the peer **by its Ed25519 key**
//!   (not by a CA chain).

use std::fmt;
use std::path::Path;

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use sha2::{Digest, Sha256};

pub mod auth;
pub mod tls;

/// Errors of this crate.
#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid identity file: {0}")]
    BadIdentityFile(&'static str),
    #[error("certificate generation failed: {0}")]
    Cert(String),
    #[error("tls: {0}")]
    Tls(#[from] rustls::Error),
    #[error("invalid key: {0}")]
    BadKey(&'static str),
    #[error("auth: {0}")]
    Auth(String),
}

pub type Result<T, E = CryptoError> = std::result::Result<T, E>;

impl From<CryptoError> for veloxdesk_core_traits::CoreError {
    fn from(e: CryptoError) -> Self {
        match e {
            CryptoError::Io(io) => Self::Io(io),
            other => Self::other(other),
        }
    }
}

/// Long-term Ed25519 identity of this device.
#[derive(Clone)]
pub struct DeviceIdentity {
    signing: SigningKey,
}

impl fmt::Debug for DeviceIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Never print the secret.
        f.debug_struct("DeviceIdentity")
            .field("device_id", &self.device_id())
            .field("fingerprint", &self.fingerprint())
            .finish()
    }
}

const IDENTITY_FILE_TAG: &str = "veloxdesk-ed25519-v1";

impl DeviceIdentity {
    /// Generate a fresh random identity (OS CSPRNG).
    pub fn generate() -> Self {
        let signing = SigningKey::generate(&mut rand_core::OsRng);
        Self { signing }
    }

    /// Build from a 32-byte Ed25519 secret seed.
    pub fn from_secret_bytes(secret: &[u8; 32]) -> Self {
        Self { signing: SigningKey::from_bytes(secret) }
    }

    /// The 32-byte secret seed (handle with care).
    pub fn secret_bytes(&self) -> [u8; 32] {
        self.signing.to_bytes()
    }

    /// Load the identity stored at `path`, or generate one and save it there.
    ///
    /// File format: one line `veloxdesk-ed25519-v1 <64 hex chars of the secret seed>`.
    /// On Unix the file is created with mode 0600.
    pub fn load_or_create(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::parse_file(&text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let id = Self::generate();
                id.save(path)?;
                Ok(id)
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Persist the identity to `path` (overwrites).
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent()
            && !dir.as_os_str().is_empty()
        {
            std::fs::create_dir_all(dir)?;
        }
        let content = format!("{IDENTITY_FILE_TAG} {}\n", hex(&self.secret_bytes()));
        let tmp = path.with_extension("tmp");
        write_private(&tmp, content.as_bytes())?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    fn parse_file(text: &str) -> Result<Self> {
        let mut parts = text.split_whitespace();
        if parts.next() != Some(IDENTITY_FILE_TAG) {
            return Err(CryptoError::BadIdentityFile("missing header tag"));
        }
        let hex_secret = parts.next().ok_or(CryptoError::BadIdentityFile("missing key"))?;
        let bytes = unhex(hex_secret).ok_or(CryptoError::BadIdentityFile("bad hex"))?;
        let secret: [u8; 32] =
            bytes.try_into().map_err(|_| CryptoError::BadIdentityFile("key must be 32 bytes"))?;
        Ok(Self::from_secret_bytes(&secret))
    }

    /// Raw Ed25519 public key.
    pub fn public_key(&self) -> [u8; 32] {
        self.signing.verifying_key().to_bytes()
    }

    /// Human-friendly 9-digit device id derived from the public key.
    pub fn device_id(&self) -> String {
        device_id_from_public_key(&self.public_key())
    }

    /// Full key fingerprint (hex SHA-256 of the public key) for out-of-band verification.
    pub fn fingerprint(&self) -> String {
        fingerprint(&self.public_key())
    }

    /// Sign an arbitrary message with the device key.
    pub fn sign(&self, msg: &[u8]) -> [u8; 64] {
        self.signing.sign(msg).to_bytes()
    }

    /// PKCS#8 v2 DER encoding of the keypair (used for the TLS certificate key).
    pub fn to_pkcs8_der(&self) -> Result<Vec<u8>> {
        use ed25519_dalek::pkcs8::EncodePrivateKey;
        let doc = self.signing.to_pkcs8_der().map_err(|_| CryptoError::BadKey("pkcs8 encode"))?;
        Ok(doc.as_bytes().to_vec())
    }
}

/// 9-digit device id (100000000..=999999999) from SHA-256 of the public key.
pub fn device_id_from_public_key(pk: &[u8; 32]) -> String {
    let h = Sha256::digest(pk);
    let mut first = [0u8; 8];
    first.copy_from_slice(&h[..8]);
    let n = u64::from_le_bytes(first) % 900_000_000 + 100_000_000;
    n.to_string()
}

/// Hex SHA-256 of a public key.
pub fn fingerprint(pk: &[u8; 32]) -> String {
    hex(&Sha256::digest(pk))
}

/// Verify an Ed25519 signature made by [`DeviceIdentity::sign`].
pub fn verify_signature(public_key: &[u8; 32], msg: &[u8], sig: &[u8; 64]) -> bool {
    let Ok(vk) = VerifyingKey::from_bytes(public_key) else {
        return false;
    };
    vk.verify(msg, &Signature::from_bytes(sig)).is_ok()
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(DIGITS[(b >> 4) as usize] as char);
        s.push(DIGITS[(b & 0xF) as usize] as char);
    }
    s
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

#[cfg(unix)]
fn write_private(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(data)?;
    f.sync_all()
}

#[cfg(not(unix))]
fn write_private(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut f = std::fs::File::create(path)?;
    f.write_all(data)?;
    f.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_id_is_nine_digits_and_stable() {
        let id = DeviceIdentity::generate();
        let d = id.device_id();
        assert_eq!(d.len(), 9);
        assert!(d.chars().all(|c| c.is_ascii_digit()));
        assert_eq!(d, device_id_from_public_key(&id.public_key()));
        let again = DeviceIdentity::from_secret_bytes(&id.secret_bytes());
        assert_eq!(again.public_key(), id.public_key());
    }

    #[test]
    fn sign_verify() {
        let id = DeviceIdentity::generate();
        let sig = id.sign(b"hello");
        assert!(verify_signature(&id.public_key(), b"hello", &sig));
        assert!(!verify_signature(&id.public_key(), b"hellO", &sig));
    }

    #[test]
    fn load_or_create_roundtrip() {
        let dir = std::env::temp_dir().join(format!("vdx-crypto-test-{}", std::process::id()));
        let path = dir.join("id.key");
        let _ = std::fs::remove_file(&path);
        let a = DeviceIdentity::load_or_create(&path).unwrap();
        let b = DeviceIdentity::load_or_create(&path).unwrap();
        assert_eq!(a.public_key(), b.public_key());
        std::fs::write(&path, "garbage").unwrap();
        assert!(DeviceIdentity::load_or_create(&path).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
