//! Password hashing (Argon2id), challenge-response authentication proofs (HMAC-SHA256,
//! Ed25519), OTP generation, and signed rendezvous registrations (ARCHITECTURE §7, M2).

use argon2::{Algorithm, Argon2, Params, Version};
use hmac::{Hmac, Mac};
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use subtle::ConstantTimeEq;

use veloxdesk_protocol::rendezvous::{NatType, RegisterPeer};

use crate::{CryptoError, DeviceIdentity, Result, device_id_from_public_key, verify_signature};

type HmacSha256 = Hmac<Sha256>;

/// Maximum accepted Argon2id memory cost (256 MiB) to prevent OOM DoS from untrusted challenges.
pub const MAX_ARGON2_M_COST_KIB: u32 = 262_144;
/// Maximum accepted Argon2id time cost (iterations) to prevent CPU DoS.
pub const MAX_ARGON2_T_COST: u32 = 16;
/// Maximum accepted Argon2id parallelism lanes to prevent thread/memory DoS.
pub const MAX_ARGON2_P_COST: u32 = 16;

/// Argon2id cost parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Argon2Params {
    pub m_cost_kib: u32,
    pub t_cost: u32,
    pub p_cost: u32,
}

fn validate_argon2_params(params: Argon2Params) -> Result<Params> {
    if params.m_cost_kib > MAX_ARGON2_M_COST_KIB {
        return Err(CryptoError::Auth(format!(
            "argon2 m_cost_kib {} exceeds max {}",
            params.m_cost_kib, MAX_ARGON2_M_COST_KIB
        )));
    }
    if params.t_cost > MAX_ARGON2_T_COST {
        return Err(CryptoError::Auth(format!(
            "argon2 t_cost {} exceeds max {}",
            params.t_cost, MAX_ARGON2_T_COST
        )));
    }
    if params.p_cost > MAX_ARGON2_P_COST {
        return Err(CryptoError::Auth(format!(
            "argon2 p_cost {} exceeds max {}",
            params.p_cost, MAX_ARGON2_P_COST
        )));
    }
    Params::new(params.m_cost_kib, params.t_cost, params.p_cost, Some(32))
        .map_err(|e| CryptoError::Auth(format!("invalid argon2 params: {e}")))
}

impl Default for Argon2Params {
    fn default() -> Self {
        // OWASP recommended minimum for Argon2id (19 MiB, 2 iterations, 1 lane).
        Self { m_cost_kib: 19_456, t_cost: 2, p_cost: 1 }
    }
}

/// Stored permanent password hash (never stores plaintext password).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PasswordHash {
    pub salt: Vec<u8>,
    pub params: Argon2Params,
    pub derived_key: [u8; 32],
}

impl PasswordHash {
    /// Hash a password with a fresh 16-byte random salt and default Argon2id parameters.
    pub fn hash(password: &str) -> Result<Self> {
        Self::hash_with(password, Argon2Params::default())
    }

    /// Hash a password with a fresh 16-byte random salt and custom Argon2id parameters.
    pub fn hash_with(password: &str, params: Argon2Params) -> Result<Self> {
        let mut salt = vec![0u8; 16];
        OsRng.fill_bytes(&mut salt);
        let derived_key = derive_password_key(password, &salt, params)?;
        Ok(Self { salt, params, derived_key })
    }

    /// Encode in standard PHC string format:
    /// `$argon2id$v=19$m=<m>,t=<t>,p=<p>$<b64(salt)>$<b64(derived_key)>`.
    pub fn to_phc_string(&self) -> String {
        format!(
            "$argon2id$v=19$m={},t={},p={}${}${}",
            self.params.m_cost_kib,
            self.params.t_cost,
            self.params.p_cost,
            b64_encode_nopad(&self.salt),
            b64_encode_nopad(&self.derived_key),
        )
    }

    /// Parse a PHC string produced by [`Self::to_phc_string`].
    pub fn from_phc_string(s: &str) -> Result<Self> {
        let parts: Vec<&str> = s.trim().split('$').collect();
        // Leading '$' yields an empty first element: ["", "argon2id", "v=19", "m=...,t=...,p=...", "<salt>", "<hash>"]
        if parts.len() != 6 || !parts[0].is_empty() || parts[1] != "argon2id" {
            return Err(CryptoError::Auth("invalid PHC argon2id string".into()));
        }
        if parts[2] != "v=19" {
            return Err(CryptoError::Auth(format!("unsupported argon2 version '{}'", parts[2])));
        }
        let (mut m, mut t, mut p) = (None, None, None);
        for kv in parts[3].split(',') {
            let (k, v) = kv
                .split_once('=')
                .ok_or_else(|| CryptoError::Auth("malformed argon2 parameter".into()))?;
            let val: u32 = v
                .parse()
                .map_err(|_| CryptoError::Auth(format!("invalid integer for parameter '{k}'")))?;
            match k {
                "m" => m = Some(val),
                "t" => t = Some(val),
                "p" => p = Some(val),
                _ => return Err(CryptoError::Auth(format!("unknown argon2 parameter '{k}'"))),
            }
        }
        let params = Argon2Params {
            m_cost_kib: m.ok_or_else(|| CryptoError::Auth("missing m parameter".into()))?,
            t_cost: t.ok_or_else(|| CryptoError::Auth("missing t parameter".into()))?,
            p_cost: p.ok_or_else(|| CryptoError::Auth("missing p parameter".into()))?,
        };
        // Validate parameters (including DoS bounds) with argon2 crate.
        validate_argon2_params(params)?;

        let salt = b64_decode_nopad(parts[4])
            .ok_or_else(|| CryptoError::Auth("invalid base64 salt in PHC string".into()))?;
        if salt.is_empty() {
            return Err(CryptoError::Auth("empty salt in PHC string".into()));
        }
        let hash_bytes = b64_decode_nopad(parts[5])
            .ok_or_else(|| CryptoError::Auth("invalid base64 hash in PHC string".into()))?;
        let derived_key: [u8; 32] = hash_bytes
            .try_into()
            .map_err(|_| CryptoError::Auth("derived key in PHC string must be 32 bytes".into()))?;

        Ok(Self { salt, params, derived_key })
    }
}

/// Derive a 32-byte key from a password and salt using Argon2id.
pub fn derive_password_key(password: &str, salt: &[u8], params: Argon2Params) -> Result<[u8; 32]> {
    let p = validate_argon2_params(params)?;
    let a2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, p);
    let mut out = [0u8; 32];
    a2.hash_password_into(password.as_bytes(), salt, &mut out)
        .map_err(|e| CryptoError::Auth(format!("argon2id derive failed: {e}")))?;
    Ok(out)
}

/// Compute `HMAC-SHA256(key = derived_key, msg = nonce)`.
pub fn password_proof(derived_key: &[u8; 32], nonce: &[u8]) -> [u8; 32] {
    let mut mac = HmacSha256::new_from_slice(derived_key).expect("HMAC accepts any key size");
    mac.update(nonce);
    mac.finalize().into_bytes().into()
}

/// Constant-time verification of a password challenge proof.
pub fn verify_password_proof(derived_key: &[u8; 32], nonce: &[u8], proof: &[u8]) -> bool {
    if proof.len() != 32 {
        return false;
    }
    let expected = password_proof(derived_key, nonce);
    bool::from(expected.ct_eq(proof))
}

/// Generate a fresh 32-byte cryptographically random challenge nonce.
pub fn generate_nonce() -> [u8; 32] {
    let mut nonce = [0u8; 32];
    OsRng.fill_bytes(&mut nonce);
    nonce
}

/// Unambiguous 32-character alphanumeric alphabet (no `0`, `O`, `1`, `I`, `l`).
const OTP_ALPHABET: &[u8; 32] = b"23456789ABCDEFGHJKLMNPQRSTUVWXYZ";

/// Generate a random one-time password of length `len` using unambiguous alphanumeric characters.
pub fn generate_otp(len: usize) -> String {
    let mut bytes = vec![0u8; len];
    OsRng.fill_bytes(&mut bytes);
    bytes.into_iter().map(|b| OTP_ALPHABET[(b & 0x1f) as usize] as char).collect()
}

/// Sign a trusted-peer public-key auth challenge: `Ed25519(nonce || host_pk)`.
pub fn sign_pubkey_challenge(id: &DeviceIdentity, nonce: &[u8], host_pk: &[u8; 32]) -> [u8; 64] {
    let mut msg = Vec::with_capacity(nonce.len() + 32);
    msg.extend_from_slice(nonce);
    msg.extend_from_slice(host_pk);
    id.sign(&msg)
}

/// Verify a trusted-peer public-key auth proof against `peer_pk`, `nonce`, and `host_pk`.
pub fn verify_pubkey_challenge(peer_pk: &[u8; 32], nonce: &[u8], host_pk: &[u8; 32], proof: &[u8]) -> bool {
    let Ok(sig): std::result::Result<[u8; 64], _> = proof.try_into() else {
        return false;
    };
    let mut msg = Vec::with_capacity(nonce.len() + 32);
    msg.extend_from_slice(nonce);
    msg.extend_from_slice(host_pk);
    verify_signature(peer_pk, &msg, &sig)
}

fn register_peer_payload(device_id: &str, public_key: &[u8; 32], timestamp_unix_ms: u64) -> Vec<u8> {
    let mut buf = Vec::with_capacity(device_id.len() + 32 + 8);
    buf.extend_from_slice(device_id.as_bytes());
    buf.extend_from_slice(public_key);
    buf.extend_from_slice(&timestamp_unix_ms.to_le_bytes());
    buf
}

/// Build and sign a `RegisterPeer` rendezvous registration message.
pub fn sign_register_peer(
    id: &DeviceIdentity,
    local_addrs: Vec<String>,
    nat_type: NatType,
    timestamp_unix_ms: u64,
) -> RegisterPeer {
    let device_id = id.device_id();
    let pk = id.public_key();
    let payload = register_peer_payload(&device_id, &pk, timestamp_unix_ms);
    let sig = id.sign(&payload);
    RegisterPeer {
        device_id,
        public_key: pk.to_vec(),
        local_addrs,
        nat_type: nat_type as i32,
        timestamp_unix_ms,
        signature: sig.to_vec(),
    }
}

/// Verify a `RegisterPeer` message's device-ID binding, timestamp freshness, and Ed25519 signature.
/// Returns the verified 32-byte Ed25519 public key.
pub fn verify_register_peer(msg: &RegisterPeer, now_unix_ms: u64, max_skew_ms: u64) -> Result<[u8; 32]> {
    let pk: [u8; 32] = msg
        .public_key
        .as_slice()
        .try_into()
        .map_err(|_| CryptoError::BadKey("RegisterPeer public_key must be 32 bytes"))?;
    let expected_id = device_id_from_public_key(&pk);
    if msg.device_id != expected_id {
        return Err(CryptoError::Auth("RegisterPeer device_id does not match public_key".into()));
    }
    if now_unix_ms.abs_diff(msg.timestamp_unix_ms) > max_skew_ms {
        return Err(CryptoError::Auth("RegisterPeer timestamp outside allowed skew".into()));
    }
    let sig: [u8; 64] = msg
        .signature
        .as_slice()
        .try_into()
        .map_err(|_| CryptoError::Auth("RegisterPeer signature must be 64 bytes".into()))?;
    let payload = register_peer_payload(&msg.device_id, &pk, msg.timestamp_unix_ms);
    if !verify_signature(&pk, &payload, &sig) {
        return Err(CryptoError::Auth("RegisterPeer invalid Ed25519 signature".into()));
    }
    Ok(pk)
}

// ───────────────────────── standard base64 (no padding) ─────────────────────────

const B64_ENC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn b64_encode_nopad(input: &[u8]) -> String {
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(B64_ENC[((n >> 18) & 0x3f) as usize] as char);
        out.push(B64_ENC[((n >> 12) & 0x3f) as usize] as char);
        if chunk.len() > 1 {
            out.push(B64_ENC[((n >> 6) & 0x3f) as usize] as char);
        }
        if chunk.len() > 2 {
            out.push(B64_ENC[(n & 0x3f) as usize] as char);
        }
    }
    out
}

fn b64_val(b: u8) -> Option<u32> {
    match b {
        b'A'..=b'Z' => Some((b - b'A') as u32),
        b'a'..=b'z' => Some((b - b'a' + 26) as u32),
        b'0'..=b'9' => Some((b - b'0' + 52) as u32),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

fn b64_decode_nopad(input: &str) -> Option<Vec<u8>> {
    let s = input.trim_end_matches('=').as_bytes();
    if s.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    for chunk in s.chunks(4) {
        let v0 = b64_val(chunk[0])?;
        let v1 = b64_val(chunk[1])?;
        out.push(((v0 << 2) | (v1 >> 4)) as u8);
        if chunk.len() > 2 {
            let v2 = b64_val(chunk[2])?;
            out.push((((v1 & 0x0f) << 4) | (v2 >> 2)) as u8);
            if chunk.len() > 3 {
                let v3 = b64_val(chunk[3])?;
                out.push((((v2 & 0x03) << 6) | v3) as u8);
            }
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fast_params() -> Argon2Params {
        Argon2Params { m_cost_kib: 1024, t_cost: 1, p_cost: 1 }
    }

    #[test]
    fn password_hash_phc_and_proof_roundtrip() {
        let ph = PasswordHash::hash_with("hunter2-secure!", fast_params()).unwrap();
        let phc = ph.to_phc_string();
        assert!(phc.starts_with("$argon2id$v=19$m=1024,t=1,p=1$"));
        let parsed = PasswordHash::from_phc_string(&phc).unwrap();
        assert_eq!(parsed, ph);

        let nonce = generate_nonce();
        let client_key = derive_password_key("hunter2-secure!", &ph.salt, ph.params).unwrap();
        let proof = password_proof(&client_key, &nonce);
        assert!(verify_password_proof(&ph.derived_key, &nonce, &proof));

        // Wrong password fails
        let wrong_key = derive_password_key("wrong-password", &ph.salt, ph.params).unwrap();
        let wrong_proof = password_proof(&wrong_key, &nonce);
        assert!(!verify_password_proof(&ph.derived_key, &nonce, &wrong_proof));

        // Wrong nonce or truncated proof fails
        let other_nonce = generate_nonce();
        assert!(!verify_password_proof(&ph.derived_key, &other_nonce, &proof));
        assert!(!verify_password_proof(&ph.derived_key, &nonce, &proof[..31]));
    }

    #[test]
    fn default_params_hash_works() {
        let ph = PasswordHash::hash("test-default-params").unwrap();
        assert_eq!(ph.params, Argon2Params::default());
        assert_eq!(ph.salt.len(), 16);
        let parsed = PasswordHash::from_phc_string(&ph.to_phc_string()).unwrap();
        assert_eq!(parsed, ph);
    }

    #[test]
    fn phc_rejects_malformed() {
        assert!(PasswordHash::from_phc_string("not-a-phc").is_err());
        assert!(PasswordHash::from_phc_string("$argon2i$v=19$m=1024,t=1,p=1$AAAA$AAAA").is_err());
        assert!(PasswordHash::from_phc_string("$argon2id$v=18$m=1024,t=1,p=1$AAAA$AAAA").is_err());
    }

    #[test]
    fn argon2_rejects_excessive_dos_params() {
        let salt = [0x11u8; 16];
        let hash_b64 = b64_encode_nopad(&[0x22u8; 32]);
        let salt_b64 = b64_encode_nopad(&salt);

        let too_much_mem = Argon2Params { m_cost_kib: MAX_ARGON2_M_COST_KIB + 1, t_cost: 2, p_cost: 1 };
        assert!(derive_password_key("pw", &salt, too_much_mem).is_err());
        let phc_mem = format!("$argon2id$v=19$m={},t=2,p=1${salt_b64}${hash_b64}", MAX_ARGON2_M_COST_KIB + 1);
        assert!(PasswordHash::from_phc_string(&phc_mem).is_err());

        let too_many_iters = Argon2Params { m_cost_kib: 1024, t_cost: MAX_ARGON2_T_COST + 1, p_cost: 1 };
        assert!(derive_password_key("pw", &salt, too_many_iters).is_err());
        let phc_t = format!("$argon2id$v=19$m=1024,t={},p=1${salt_b64}${hash_b64}", MAX_ARGON2_T_COST + 1);
        assert!(PasswordHash::from_phc_string(&phc_t).is_err());

        let too_many_lanes = Argon2Params { m_cost_kib: 1024, t_cost: 1, p_cost: MAX_ARGON2_P_COST + 1 };
        assert!(derive_password_key("pw", &salt, too_many_lanes).is_err());
        let phc_p = format!("$argon2id$v=19$m=1024,t=1,p={}${salt_b64}${hash_b64}", MAX_ARGON2_P_COST + 1);
        assert!(PasswordHash::from_phc_string(&phc_p).is_err());
    }

    #[test]
    fn otp_is_unambiguous() {
        let otp = generate_otp(64);
        assert_eq!(otp.len(), 64);
        for c in otp.chars() {
            assert!(c.is_ascii_alphanumeric());
            assert!(!matches!(c, '0' | 'O' | '1' | 'I' | 'l'), "ambiguous char {c}");
        }
        assert_ne!(generate_otp(12), generate_otp(12));
    }

    #[test]
    fn pubkey_challenge_roundtrip_and_binding() {
        let viewer = DeviceIdentity::generate();
        let host = DeviceIdentity::generate();
        let other_host = DeviceIdentity::generate();
        let nonce = generate_nonce();

        let sig = sign_pubkey_challenge(&viewer, &nonce, &host.public_key());
        assert!(verify_pubkey_challenge(&viewer.public_key(), &nonce, &host.public_key(), &sig));
        // Bound to host_pk: fails for another host
        assert!(!verify_pubkey_challenge(&viewer.public_key(), &nonce, &other_host.public_key(), &sig));
        // Bound to nonce: fails for another nonce
        assert!(!verify_pubkey_challenge(&viewer.public_key(), &generate_nonce(), &host.public_key(), &sig));
        // Bound to viewer key
        assert!(!verify_pubkey_challenge(&host.public_key(), &nonce, &host.public_key(), &sig));
    }

    #[test]
    fn register_peer_sign_verify_and_tamper_rejection() {
        let host = DeviceIdentity::generate();
        let now = 1_700_000_000_000u64;
        let msg = sign_register_peer(&host, vec!["192.168.1.5:21118".into()], NatType::Cone, now);

        let verified_pk = verify_register_peer(&msg, now + 5_000, 60_000).unwrap();
        assert_eq!(verified_pk, host.public_key());

        // Clock skew rejected
        assert!(verify_register_peer(&msg, now + 120_000, 60_000).is_err());

        // Mismatched device_id rejected
        let mut bad_id = msg.clone();
        bad_id.device_id = "999999999".into();
        assert!(verify_register_peer(&bad_id, now, 60_000).is_err());

        // Forged signature rejected
        let mut bad_sig = msg.clone();
        bad_sig.signature[0] ^= 0xff;
        assert!(verify_register_peer(&bad_sig, now, 60_000).is_err());
    }
}
