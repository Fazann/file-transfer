//! TLS settings for QUIC, which always encrypts.
//!
//! **Development only.** The server uses a throwaway self-signed certificate and the client accepts any certificate,
//! so this encrypts but does not authenticate. Phase 5 deletes both functions and adds real verification.

use std::{io, sync::Arc};

use rustls::{
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    crypto::{verify_tls12_signature, verify_tls13_signature, CryptoProvider},
    pki_types::{CertificateDer, PrivatePkcs8KeyDer, ServerName, UnixTime},
    version::TLS13,
    ClientConfig, DigitallySignedStruct, ServerConfig, SignatureScheme,
};

use crate::error::{Error, Result};

/// Server config with a new self-signed certificate. Development only, see the module docs.
pub(crate) fn dev_server_config() -> Result<ServerConfig> {
    let certified = rcgen::generate_simple_self_signed(vec!["lanshare".to_owned()]).map_err(tls_error)?;
    let certificate = certified.cert.der().clone();
    let key = PrivatePkcs8KeyDer::from(certified.signing_key.serialize_der());
    ServerConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&TLS13])
        .map_err(tls_error)?
        .with_no_client_auth()
        .with_single_cert(vec![certificate], key.into())
        .map_err(tls_error)
}

/// Client config that accepts any server certificate. Development only, see the module docs.
pub(crate) fn dev_insecure_client_config() -> Result<ClientConfig> {
    let provider = provider();
    Ok(ClientConfig::builder_with_provider(Arc::clone(&provider))
        .with_protocol_versions(&[&TLS13])
        .map_err(tls_error)?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAnyCertificate(provider)))
        .with_no_client_auth())
}

/// The `ring` crypto backend. Picked explicitly so no process-wide default has to be installed.
fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

fn tls_error(error: impl std::error::Error + Send + Sync + 'static) -> Error {
    Error::Io(io::Error::other(error))
}

/// Accepts every certificate, but still checks handshake signatures so the TLS session itself is sound.
#[derive(Debug)]
struct AcceptAnyCertificate(Arc<CryptoProvider>);

impl ServerCertVerifier for AcceptAnyCertificate {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
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
