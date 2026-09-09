//! Identity-pinned TLS: no certificate authority, no hostnames. Each peer
//! presents a self-signed certificate derived from its Ed25519 device
//! keypair; the custom verifiers below extract that embedded public key
//! and accept the connection only if it's in the caller-supplied trust
//! store. The certificate is not a durable identity — it's a disposable
//! TLS artifact regenerated at startup from the real identity (the
//! `identity` crate's keypair).

use std::fmt;
use std::sync::Arc;

use ed25519_dalek::pkcs8::{DecodePublicKey, EncodePrivateKey};
use ed25519_dalek::{SigningKey, VerifyingKey};
use kvm_protocol::DeviceId;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{DigitallySignedStruct, DistinguishedName, Error as TlsError, SignatureScheme};

use crate::error::NetError;

/// Callback deciding whether a peer's device identity is trusted. Kept as
/// a plain closure (not a trait tied to `identity::TrustStore`) so this
/// crate doesn't need to know how or where trust is persisted.
pub type TrustCheck = Arc<dyn Fn(&DeviceId) -> bool + Send + Sync>;

/// A self-signed identity certificate and its private key, derived from a
/// device's Ed25519 keypair seed.
pub struct IdentityCert {
    pub cert_der: CertificateDer<'static>,
    pub key_der: PrivatePkcs8KeyDer<'static>,
}

impl IdentityCert {
    /// Builds a fresh self-signed certificate from a device's private key
    /// seed (the same seed persisted by `identity::DeviceKeypair`).
    pub fn from_seed(seed: &[u8; 32]) -> Result<Self, NetError> {
        let signing_key = SigningKey::from_bytes(seed);
        let pkcs8_doc = signing_key
            .to_pkcs8_der()
            .map_err(|e| NetError::Identity(e.to_string()))?;
        let key_der = PrivatePkcs8KeyDer::from(pkcs8_doc.as_bytes().to_vec());

        let rcgen_kp = rcgen::KeyPair::from_pkcs8_der_and_sign_algo(&key_der, &rcgen::PKCS_ED25519)
            .map_err(|e| NetError::Identity(e.to_string()))?;
        let cert = rcgen::CertificateParams::default()
            .self_signed(&rcgen_kp)
            .map_err(|e| NetError::Identity(e.to_string()))?;

        Ok(Self {
            cert_der: cert.der().clone(),
            key_der,
        })
    }
}

/// Extracts the Ed25519 public key embedded in a certificate's
/// SubjectPublicKeyInfo — this *is* the peer's [`DeviceId`].
pub(crate) fn extract_device_id(cert: &CertificateDer<'_>) -> Result<DeviceId, TlsError> {
    let parsed = rustls::server::ParsedCertificate::try_from(cert)
        .map_err(|_| TlsError::InvalidCertificate(rustls::CertificateError::BadEncoding))?;
    let spki = parsed.subject_public_key_info();
    let verifying_key = VerifyingKey::from_public_key_der(spki.as_ref())
        .map_err(|_| TlsError::InvalidCertificate(rustls::CertificateError::BadEncoding))?;
    Ok(verifying_key.to_bytes())
}

/// Verifies a device's pinned identity, rejecting anything else — shared
/// logic between the client and server verifier roles.
fn verify_pinned_identity(
    cert: &CertificateDer<'_>,
    trust_check: &TrustCheck,
) -> Result<(), TlsError> {
    let device_id = extract_device_id(cert)?;
    if trust_check(&device_id) {
        Ok(())
    } else {
        Err(TlsError::InvalidCertificate(
            rustls::CertificateError::ApplicationVerificationFailure,
        ))
    }
}

/// Verifies a QUIC server's certificate (used when this device dials out).
pub struct PinnedServerVerifier {
    provider: Arc<CryptoProvider>,
    trust_check: TrustCheck,
}

impl PinnedServerVerifier {
    pub fn new(provider: Arc<CryptoProvider>, trust_check: TrustCheck) -> Self {
        Self {
            provider,
            trust_check,
        }
    }
}

impl fmt::Debug for PinnedServerVerifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PinnedServerVerifier")
            .finish_non_exhaustive()
    }
}

impl ServerCertVerifier for PinnedServerVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, TlsError> {
        verify_pinned_identity(end_entity, &self.trust_check)?;
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
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
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![SignatureScheme::ED25519]
    }
}

/// Verifies a QUIC client's certificate (used when this device accepts an
/// incoming connection). Client auth is mandatory: an unauthenticated
/// client is never accepted.
pub struct PinnedClientVerifier {
    provider: Arc<CryptoProvider>,
    trust_check: TrustCheck,
}

impl PinnedClientVerifier {
    pub fn new(provider: Arc<CryptoProvider>, trust_check: TrustCheck) -> Self {
        Self {
            provider,
            trust_check,
        }
    }
}

impl fmt::Debug for PinnedClientVerifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PinnedClientVerifier")
            .finish_non_exhaustive()
    }
}

impl ClientCertVerifier for PinnedClientVerifier {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, TlsError> {
        verify_pinned_identity(end_entity, &self.trust_check)?;
        Ok(ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
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
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![SignatureScheme::ED25519]
    }

    fn client_auth_mandatory(&self) -> bool {
        true
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn identity_cert_embeds_the_expected_device_id() {
        let seed = [3u8; 32];
        let cert = IdentityCert::from_seed(&seed).unwrap();

        let expected_device_id = SigningKey::from_bytes(&seed).verifying_key().to_bytes();
        let extracted = extract_device_id(&cert.cert_der).unwrap();
        assert_eq!(extracted, expected_device_id);
    }

    #[test]
    fn different_seeds_produce_different_device_ids() {
        let cert_a = IdentityCert::from_seed(&[1u8; 32]).unwrap();
        let cert_b = IdentityCert::from_seed(&[2u8; 32]).unwrap();

        let id_a = extract_device_id(&cert_a.cert_der).unwrap();
        let id_b = extract_device_id(&cert_b.cert_der).unwrap();
        assert_ne!(id_a, id_b);
    }

    #[test]
    fn verify_pinned_identity_accepts_trusted_device() {
        let cert = IdentityCert::from_seed(&[5u8; 32]).unwrap();
        let device_id = extract_device_id(&cert.cert_der).unwrap();
        let trust_check: TrustCheck = Arc::new(move |id| *id == device_id);

        verify_pinned_identity(&cert.cert_der, &trust_check).unwrap();
    }

    #[test]
    fn verify_pinned_identity_rejects_untrusted_device() {
        let cert = IdentityCert::from_seed(&[6u8; 32]).unwrap();
        let trust_check: TrustCheck = Arc::new(|_id| false);

        let err = verify_pinned_identity(&cert.cert_der, &trust_check).unwrap_err();
        assert!(matches!(
            err,
            TlsError::InvalidCertificate(rustls::CertificateError::ApplicationVerificationFailure)
        ));
    }
}
