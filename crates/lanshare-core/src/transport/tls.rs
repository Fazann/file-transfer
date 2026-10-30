//! TLS 1.3 settings for both transports. Both sides present their device certificate, so after the handshake each
//! knows the other's [`Fingerprint`] and that the other holds the matching private key.
//!
//! - The **sender** (TLS client) checks the receiver inside the handshake, as [`PeerCheck`] says: the fingerprint
//!   must match the one discovery advertised, and must be trusted unless the connection is for pairing.
//! - The **receiver** (TLS server) cannot know yet whether the sender wants to pair, so its verifier only checks that
//!   the sender holds its key. The receiver applies its trust policy right after the handshake, before it accepts
//!   anything (see `transfer::receiver`).
//!
//! Certificates are self-signed, so no names, dates or certificate authorities are checked: a device is its key.

use std::{
    io,
    sync::{Arc, Mutex, PoisonError},
};

use rustls::{
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    crypto::{verify_tls12_signature, verify_tls13_signature, CryptoProvider},
    pki_types::{CertificateDer, ServerName, UnixTime},
    server::danger::{ClientCertVerified, ClientCertVerifier},
    version::TLS13,
    CertificateError, ClientConfig, DigitallySignedStruct, DistinguishedName, ServerConfig, SignatureScheme,
};
use tracing::warn;

use crate::{
    error::{Error, Result},
    identity::{DeviceCertificate, Fingerprint, TrustStore},
};

/// Server name the client asks for. Certificates are recognized by fingerprint, so the name is never checked.
pub(super) const SERVER_NAME: &str = "lanshare";

/// Label for the TLS exporter (RFC 5705) value that pairing binds to.
const EXPORTER_LABEL: &[u8] = b"EXPORTER-lanshare-channel-binding";

/// How the sender checks the receiver's certificate.
#[derive(Debug, Clone)]
pub enum PeerCheck {
    /// The receiver's fingerprint must be in this trust store. Otherwise connecting fails with
    /// [`Error::UntrustedDevice`].
    Trusted(TrustStore),
    /// Any receiver is accepted, so the two devices can pair. Nothing but pairing messages may be sent.
    Pairing,
}

/// Why the sender's verifier refused a receiver. Read after a failed handshake to report the right error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Refusal {
    Untrusted,
    Mismatch,
}

/// Where the verifier leaves its [`Refusal`]. A failed handshake only reports a generic TLS error.
#[derive(Debug, Clone, Default)]
pub(super) struct RefusalSlot(Arc<Mutex<Option<Refusal>>>);

impl RefusalSlot {
    fn set(&self, refusal: Refusal) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = Some(refusal);
    }

    /// The error to report for a failed handshake: the verifier's refusal if it made one, else `fallback`.
    pub(super) fn error_or(&self, fallback: Error) -> Error {
        match *self.0.lock().unwrap_or_else(PoisonError::into_inner) {
            Some(Refusal::Untrusted) => Error::UntrustedDevice,
            Some(Refusal::Mismatch) => Error::FingerprintMismatch,
            None => fallback,
        }
    }
}

/// Receiver config: presents this device's certificate and requires one from every sender.
pub(super) fn server_config(certificate: &DeviceCertificate) -> Result<ServerConfig> {
    let verifier = Arc::new(KeyHolderVerifier(provider()));
    let mut config = ServerConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&TLS13])
        .map_err(tls_error)?
        .with_client_cert_verifier(verifier)
        .with_single_cert(vec![certificate.certificate()], certificate.private_key())
        .map_err(tls_error)?;
    // No session tickets: nothing resumes sessions, and a ticket nobody reads on a write-only TCP data connection
    // makes the sender's operating system reset the connection when it closes, which can drop the last chunk.
    config.send_tls13_tickets = 0;
    Ok(config)
}

/// Sender config: presents this device's certificate and checks the receiver's as `check` and `expected` say.
///
/// The returned slot tells why a handshake was refused.
pub(super) fn client_config(
    certificate: &DeviceCertificate,
    check: &PeerCheck,
    expected: Option<Fingerprint>,
) -> Result<(ClientConfig, RefusalSlot)> {
    let refusal = RefusalSlot::default();
    let verifier =
        Arc::new(ReceiverVerifier { provider: provider(), check: check.clone(), expected, refusal: refusal.clone() });
    let config = ClientConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&TLS13])
        .map_err(tls_error)?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_client_auth_cert(vec![certificate.certificate()], certificate.private_key())
        .map_err(tls_error)?;
    Ok((config, refusal))
}

/// Fingerprint of the first certificate the peer presented.
pub(super) fn peer_fingerprint(certificates: Option<&[CertificateDer<'_>]>) -> Result<Fingerprint> {
    let certificate = certificates.and_then(<[_]>::first).ok_or(Error::FingerprintMismatch)?;
    Fingerprint::of_certificate(certificate)
}

/// Fills a 32-byte channel binding value from a TLS exporter function, which both ends compute the same way.
pub(super) fn channel_binding<E: std::fmt::Debug>(
    export: impl FnOnce(&mut [u8; 32], &[u8]) -> std::result::Result<(), E>,
) -> Result<[u8; 32]> {
    let mut binding = [0; 32];
    export(&mut binding, EXPORTER_LABEL).map_err(|error| Error::Io(io::Error::other(format!("{error:?}"))))?;
    Ok(binding)
}

/// The `ring` crypto backend. Picked explicitly so no process-wide default has to be installed.
fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

fn tls_error(error: impl std::error::Error + Send + Sync + 'static) -> Error {
    Error::Io(io::Error::other(error))
}

/// The sender's check of the receiver's certificate.
#[derive(Debug)]
struct ReceiverVerifier {
    provider: Arc<CryptoProvider>,
    check: PeerCheck,
    expected: Option<Fingerprint>,
    refusal: RefusalSlot,
}

impl ServerCertVerifier for ReceiverVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        let refuse = |refusal: Refusal| {
            self.refusal.set(refusal);
            Err(rustls::Error::InvalidCertificate(CertificateError::ApplicationVerificationFailure))
        };
        let Ok(fingerprint) = Fingerprint::of_certificate(end_entity) else {
            return Err(rustls::Error::InvalidCertificate(CertificateError::BadEncoding));
        };
        if self.expected.is_some_and(|expected| expected != fingerprint) {
            warn!(%fingerprint, expected = ?self.expected, "receiver's certificate differs from the advertised one");
            return refuse(Refusal::Mismatch);
        }
        match &self.check {
            PeerCheck::Trusted(store) if store.get_by_fingerprint(&fingerprint).is_none() => refuse(Refusal::Untrusted),
            PeerCheck::Trusted(_) | PeerCheck::Pairing => Ok(ServerCertVerified::assertion()),
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        certificate: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(message, certificate, signature, &self.provider.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        certificate: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, certificate, signature, &self.provider.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

/// The receiver's check of the sender's certificate: it must parse, and the handshake signature proves the sender
/// holds its key. Whether the sender is trusted is decided after the handshake.
#[derive(Debug)]
struct KeyHolderVerifier(Arc<CryptoProvider>);

impl ClientCertVerifier for KeyHolderVerifier {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> std::result::Result<ClientCertVerified, rustls::Error> {
        Fingerprint::of_certificate(end_entity)
            .map(|_| ClientCertVerified::assertion())
            .map_err(|_| rustls::Error::InvalidCertificate(CertificateError::BadEncoding))
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        certificate: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(message, certificate, signature, &self.0.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        certificate: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, certificate, signature, &self.0.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}
