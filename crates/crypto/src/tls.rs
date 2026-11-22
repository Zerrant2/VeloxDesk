//! TLS 1.3 material bound to the device identity (ARCHITECTURE ADR-003, §7).
//!
//! Each device presents a self-signed X.509 certificate whose subject key **is** its
//! Ed25519 device key (the certificate is signed by that same key). Peers do not use CA
//! chains: the custom verifiers accept a certificate iff its Ed25519 key satisfies the
//! [`PeerPolicy`], and rustls then checks the TLS 1.3 `CertificateVerify` signature with
//! that key — proving possession of the device private key. Both sides authenticate
//! (mutual TLS), so the host learns the viewer's key for whitelisting.

use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, WebPkiSupportedAlgorithms};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{CertificateError, DigitallySignedStruct, DistinguishedName, SignatureScheme};

use crate::{CryptoError, DeviceIdentity, Result};

/// SNI used for peer connections (identity is checked by key, not by name).
pub const PEER_SERVER_NAME: &str = "peer.veloxdesk";

/// Which peer keys a verifier accepts.
#[derive(Debug, Clone, Default)]
pub enum PeerPolicy {
    /// Accept any valid self-signed Ed25519 certificate (M1 LAN / first contact; the
    /// application authorizes afterwards using the reported key + password).
    #[default]
    Any,
    /// Accept only this exact Ed25519 public key.
    Exact([u8; 32]),
    /// Accept any key from the list.
    AllowList(Vec<[u8; 32]>),
}

impl PeerPolicy {
    pub fn from_expected(expected: Option<[u8; 32]>) -> Self {
        expected.map_or(Self::Any, Self::Exact)
    }
    pub fn accepts(&self, key: &[u8; 32]) -> bool {
        match self {
            Self::Any => true,
            Self::Exact(k) => k == key,
            Self::AllowList(list) => list.iter().any(|k| k == key),
        }
    }
}

/// The rustls crypto provider used everywhere (ring, TLS 1.3 suites).
pub fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// Self-signed certificate + private key derived from the identity.
pub fn certificate(id: &DeviceIdentity) -> Result<(CertificateDer<'static>, PrivateKeyDer<'static>)> {
    let pkcs8 = id.to_pkcs8_der()?;
    let pkcs8_der = PrivatePkcs8KeyDer::from(pkcs8);
    let key_pair = rcgen::KeyPair::try_from(&pkcs8_der).map_err(|e| CryptoError::Cert(e.to_string()))?;
    let mut params = rcgen::CertificateParams::new(vec![PEER_SERVER_NAME.to_string()])
        .map_err(|e| CryptoError::Cert(e.to_string()))?;
    let mut dn = rcgen::DistinguishedName::new();
    dn.push(rcgen::DnType::CommonName, format!("VeloxDesk {}", id.device_id()));
    params.distinguished_name = dn;
    params.not_before = rcgen::date_time_ymd(2020, 1, 1);
    params.not_after = rcgen::date_time_ymd(2120, 1, 1);
    let cert = params.self_signed(&key_pair).map_err(|e| CryptoError::Cert(e.to_string()))?;
    Ok((cert.der().clone(), PrivateKeyDer::Pkcs8(pkcs8_der)))
}

/// Extract the Ed25519 public key from a DER certificate.
pub fn public_key_from_cert(der: &[u8]) -> Result<[u8; 32]> {
    let (_, cert) =
        x509_parser::parse_x509_certificate(der).map_err(|_| CryptoError::BadKey("unparsable certificate"))?;
    let spki = cert.public_key();
    if spki.algorithm.algorithm != x509_parser::oid_registry::OID_SIG_ED25519 {
        return Err(CryptoError::BadKey("certificate key is not Ed25519"));
    }
    let raw: &[u8] = spki.subject_public_key.data.as_ref();
    raw.try_into().map_err(|_| CryptoError::BadKey("Ed25519 key must be 32 bytes"))
}

/// rustls server config (TLS 1.3 only, mandatory client certificate checked by `clients`).
pub fn server_config(
    id: &DeviceIdentity,
    alpn: &[&[u8]],
    clients: PeerPolicy,
) -> Result<rustls::ServerConfig> {
    let provider = provider();
    let (cert, key) = certificate(id)?;
    let verifier = Arc::new(PeerKeyVerifier::new(clients, &provider));
    let mut cfg = rustls::ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_client_cert_verifier(verifier)
        .with_single_cert(vec![cert], key)?;
    cfg.alpn_protocols = alpn.iter().map(|a| a.to_vec()).collect();
    Ok(cfg)
}

/// rustls client config (TLS 1.3 only) presenting our certificate and accepting the
/// server iff its key satisfies `server`.
pub fn client_config(
    id: &DeviceIdentity,
    alpn: &[&[u8]],
    server: PeerPolicy,
) -> Result<rustls::ClientConfig> {
    let provider = provider();
    let (cert, key) = certificate(id)?;
    let verifier = Arc::new(PeerKeyVerifier::new(server, &provider));
    let mut cfg = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_client_auth_cert(vec![cert], key)?;
    cfg.alpn_protocols = alpn.iter().map(|a| a.to_vec()).collect();
    Ok(cfg)
}

/// Verifier authenticating a peer by its Ed25519 certificate key.
#[derive(Debug)]
pub struct PeerKeyVerifier {
    policy: PeerPolicy,
    algs: WebPkiSupportedAlgorithms,
}

impl PeerKeyVerifier {
    pub fn new(policy: PeerPolicy, provider: &CryptoProvider) -> Self {
        Self { policy, algs: provider.signature_verification_algorithms }
    }

    fn check(&self, end_entity: &CertificateDer<'_>, intermediates: &[CertificateDer<'_>]) -> Result<(), rustls::Error> {
        if !intermediates.is_empty() {
            return Err(rustls::Error::InvalidCertificate(CertificateError::BadEncoding));
        }
        let key = public_key_from_cert(end_entity)
            .map_err(|_| rustls::Error::InvalidCertificate(CertificateError::BadEncoding))?;
        if self.policy.accepts(&key) {
            Ok(())
        } else {
            Err(rustls::Error::InvalidCertificate(CertificateError::ApplicationVerificationFailure))
        }
    }

    fn verify13(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        if dss.scheme != SignatureScheme::ED25519 {
            return Err(rustls::Error::PeerMisbehaved(rustls::PeerMisbehaved::SignedHandshakeWithUnadvertisedSigScheme));
        }
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.algs)
    }
}

impl ServerCertVerifier for PeerKeyVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        self.check(end_entity, intermediates)?;
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::PeerIncompatible(rustls::PeerIncompatible::Tls13RequiredForQuic))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.verify13(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![SignatureScheme::ED25519]
    }
}

impl ClientCertVerifier for PeerKeyVerifier {
    fn offer_client_auth(&self) -> bool {
        true
    }

    fn client_auth_mandatory(&self) -> bool {
        true
    }

    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, rustls::Error> {
        self.check(end_entity, intermediates)?;
        Ok(ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::PeerIncompatible(rustls::PeerIncompatible::Tls13RequiredForQuic))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.verify13(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![SignatureScheme::ED25519]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cert_carries_identity_key() {
        let id = DeviceIdentity::generate();
        let (cert, _key) = certificate(&id).unwrap();
        assert_eq!(public_key_from_cert(cert.as_ref()).unwrap(), id.public_key());
    }

    #[test]
    fn configs_build() {
        let id = DeviceIdentity::generate();
        server_config(&id, &[b"veloxdesk/1"], PeerPolicy::Any).unwrap();
        client_config(&id, &[b"veloxdesk/1"], PeerPolicy::Exact([7; 32])).unwrap();
    }

    #[test]
    fn policy() {
        assert!(PeerPolicy::Any.accepts(&[1; 32]));
        assert!(PeerPolicy::Exact([1; 32]).accepts(&[1; 32]));
        assert!(!PeerPolicy::Exact([1; 32]).accepts(&[2; 32]));
        assert!(PeerPolicy::AllowList(vec![[3; 32], [2; 32]]).accepts(&[2; 32]));
    }
}
