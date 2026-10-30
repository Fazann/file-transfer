//! This device's key pair and self-signed certificate, and the [`Fingerprint`] that identifies any device's key.
//!
//! The certificate is only a container for the public key: nobody checks its name or dates. A device is recognized by
//! the SHA-256 hash of its public key, which stays the same as long as `device.key` is kept.

use std::{
    fmt::{self, Write as _},
    fs,
    io::{self, Write},
    path::Path,
    sync::Arc,
};

use rcgen::{CertificateParams, KeyPair, PKCS_ED25519};
use ring::digest::{digest, SHA256};
use rustls::pki_types::{pem::PemObject, CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use serde::{de, Deserialize, Deserializer, Serialize, Serializer};

use crate::{
    error::{Error, Result},
    paths,
};

/// File name of the private key inside [`paths::config_dir`].
pub const KEY_FILE_NAME: &str = "device.key";

/// File name of the certificate inside [`paths::config_dir`].
pub const CERTIFICATE_FILE_NAME: &str = "device.crt";

/// Name written into the certificate. Nothing checks it.
const CERTIFICATE_NAME: &str = "lanshare";

/// SHA-256 of a device's public key (the DER `SubjectPublicKeyInfo` from its certificate).
///
/// `Display` prints the short form people compare by eye, e.g. `A1B2-C3D4-E5F6`. Config files store the full hex
/// form; the wire format stores the 32 raw bytes.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Fingerprint([u8; 32]);

impl Fingerprint {
    /// Wraps 32 raw bytes.
    #[must_use]
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// The 32 raw bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Fingerprint of the public key inside a DER certificate.
    ///
    /// # Errors
    ///
    /// [`Error::FingerprintMismatch`] if the certificate cannot be parsed, so it cannot belong to any device.
    pub fn of_certificate(certificate: &CertificateDer<'_>) -> Result<Self> {
        let parsed = webpki::EndEntityCert::try_from(certificate).map_err(|_| Error::FingerprintMismatch)?;
        let hash = digest(&SHA256, parsed.subject_public_key_info().as_ref());
        let mut bytes = [0; 32];
        bytes.copy_from_slice(hash.as_ref());
        Ok(Self(bytes))
    }

    /// All 64 hex digits, lowercase. Used in `trusted_devices.toml` and the mDNS TXT record.
    #[must_use]
    pub fn to_hex(&self) -> String {
        self.0.iter().fold(String::with_capacity(64), |mut hex, byte| {
            // Writing into a `String` cannot fail.
            let _ = write!(hex, "{byte:02x}");
            hex
        })
    }

    /// Parses the 64-digit hex form from [`Fingerprint::to_hex`], in either case.
    #[must_use]
    pub fn parse_hex(text: &str) -> Option<Self> {
        if text.len() != 64 || !text.is_ascii() {
            return None;
        }
        let mut bytes = [0; 32];
        for (byte, pair) in bytes.iter_mut().zip(text.as_bytes().chunks(2)) {
            // `pair` is ASCII, so it is valid UTF-8.
            *byte = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
        }
        Some(Self(bytes))
    }

    /// The first 12 hex digits in groups of four, uppercase: `A1B2-C3D4-E5F6`.
    #[must_use]
    pub fn short(&self) -> String {
        let hex = self.to_hex().to_uppercase();
        format!("{}-{}-{}", &hex[0..4], &hex[4..8], &hex[8..12])
    }
}

impl fmt::Display for Fingerprint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.short())
    }
}

impl fmt::Debug for Fingerprint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "Fingerprint({})", self.short())
    }
}

/// Text formats (TOML, JSON) get the hex string; binary formats (`postcard`) get the raw bytes.
impl Serialize for Fingerprint {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        if serializer.is_human_readable() {
            serializer.serialize_str(&self.to_hex())
        } else {
            self.0.serialize(serializer)
        }
    }
}

impl<'de> Deserialize<'de> for Fingerprint {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        if deserializer.is_human_readable() {
            let text = String::deserialize(deserializer)?;
            Self::parse_hex(&text).ok_or_else(|| de::Error::custom("expected 64 hex digits"))
        } else {
            <[u8; 32]>::deserialize(deserializer).map(Self)
        }
    }
}

/// This device's Ed25519 key pair and self-signed certificate.
///
/// Cloning is cheap: the key and certificate are shared behind an `Arc`.
#[derive(Clone)]
pub struct DeviceCertificate {
    inner: Arc<Inner>,
}

struct Inner {
    certificate: CertificateDer<'static>,
    key: PrivatePkcs8KeyDer<'static>,
    fingerprint: Fingerprint,
}

impl fmt::Debug for DeviceCertificate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Never print the private key, not even in debug logs.
        formatter.debug_struct("DeviceCertificate").field("fingerprint", &self.inner.fingerprint).finish()
    }
}

impl DeviceCertificate {
    /// Generates a new key pair and certificate in memory, without saving them.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] if key generation fails.
    pub fn generate() -> Result<Self> {
        Self::generate_with_pem().map(|(created, _pem)| created)
    }

    /// Generates a key pair and certificate, also returning them as PEM text (key, then certificate) for saving.
    fn generate_with_pem() -> Result<(Self, (String, String))> {
        let key = KeyPair::generate_for(&PKCS_ED25519).map_err(crypto_error)?;
        let certificate = CertificateParams::new(vec![CERTIFICATE_NAME.to_owned()])
            .and_then(|params| params.self_signed(&key))
            .map_err(crypto_error)?;
        let pem = (key.serialize_pem(), certificate.pem());
        let created = Self::from_parts(certificate.der().clone(), PrivatePkcs8KeyDer::from(key.serialize_der()))?;
        Ok((created, pem))
    }

    /// Loads `device.key` and `device.crt` from [`paths::config_dir`], creating them on first run.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] if the files cannot be read, parsed or written.
    pub async fn load_or_create() -> Result<Self> {
        tokio::task::spawn_blocking(|| Self::load_or_create_in(&paths::config_dir()?))
            .await
            .map_err(|error| Error::Io(io::Error::other(error)))?
    }

    /// Loads the key and certificate from `dir`, creating both if either is missing. Blocking.
    ///
    /// The key file is written with permissions `0600` on Unix, so other users cannot read it.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] if the files cannot be read, parsed or written.
    pub fn load_or_create_in(dir: &Path) -> Result<Self> {
        let key_path = dir.join(KEY_FILE_NAME);
        let certificate_path = dir.join(CERTIFICATE_FILE_NAME);
        if key_path.exists() && certificate_path.exists() {
            let key = PrivatePkcs8KeyDer::from_pem_file(&key_path).map_err(|error| invalid_file(&key_path, &error))?;
            let certificate = CertificateDer::from_pem_file(&certificate_path)
                .map_err(|error| invalid_file(&certificate_path, &error))?;
            return Self::from_parts(certificate, key);
        }
        let (created, (key_pem, certificate_pem)) = Self::generate_with_pem()?;
        write_private(&key_path, key_pem.as_bytes())?;
        fs::write(&certificate_path, certificate_pem).map_err(Error::Io)?;
        Ok(created)
    }

    fn from_parts(certificate: CertificateDer<'static>, key: PrivatePkcs8KeyDer<'static>) -> Result<Self> {
        let fingerprint = Fingerprint::of_certificate(&certificate)
            .map_err(|_| Error::Io(io::Error::new(io::ErrorKind::InvalidData, "unreadable device certificate")))?;
        Ok(Self { inner: Arc::new(Inner { certificate, key, fingerprint }) })
    }

    /// This device's fingerprint.
    #[must_use]
    pub fn fingerprint(&self) -> Fingerprint {
        self.inner.fingerprint
    }

    /// The DER certificate, for TLS.
    pub(crate) fn certificate(&self) -> CertificateDer<'static> {
        self.inner.certificate.clone()
    }

    /// A copy of the private key, for TLS.
    pub(crate) fn private_key(&self) -> PrivateKeyDer<'static> {
        PrivateKeyDer::Pkcs8(self.inner.key.clone_key())
    }
}

/// Creates `path` readable only by its owner, then writes `contents`. Blocking.
fn write_private(path: &Path, contents: &[u8]) -> Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        // `mode` only exists on Unix, like a build-tagged file in Go.
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(Error::Io)?;
    file.write_all(contents).map_err(Error::Io)?;
    file.sync_all().map_err(Error::Io)
}

fn crypto_error(error: impl std::error::Error + Send + Sync + 'static) -> Error {
    Error::Io(io::Error::other(error))
}

fn invalid_file(path: &Path, error: &dyn std::error::Error) -> Error {
    Error::Io(io::Error::new(io::ErrorKind::InvalidData, format!("{}: {error}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn certificate_is_created_once_and_reloaded() {
        let dir = tempfile::tempdir().unwrap();
        let created = DeviceCertificate::load_or_create_in(dir.path()).unwrap();
        let loaded = DeviceCertificate::load_or_create_in(dir.path()).unwrap();
        assert_eq!(created.fingerprint(), loaded.fingerprint());
        assert_eq!(created.certificate(), loaded.certificate());
        assert_ne!(created.fingerprint(), DeviceCertificate::generate().unwrap().fingerprint());
    }

    #[cfg(unix)]
    #[test]
    fn key_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        DeviceCertificate::load_or_create_in(dir.path()).unwrap();
        let mode = fs::metadata(dir.path().join(KEY_FILE_NAME)).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn fingerprint_forms() {
        let fingerprint = Fingerprint::from_bytes([0xab; 32]);
        assert_eq!(fingerprint.short(), "ABAB-ABAB-ABAB");
        assert_eq!(fingerprint.to_string(), "ABAB-ABAB-ABAB");
        assert_eq!(Fingerprint::parse_hex(&fingerprint.to_hex()), Some(fingerprint));
        assert_eq!(Fingerprint::parse_hex(&fingerprint.to_hex().to_uppercase()), Some(fingerprint));
        assert_eq!(Fingerprint::parse_hex("abc"), None);
        assert_eq!(Fingerprint::parse_hex(&"zz".repeat(32)), None);
    }

    #[test]
    fn fingerprint_serializes_as_hex_in_text_and_bytes_on_the_wire() {
        let fingerprint = Fingerprint::from_bytes([7; 32]);
        let json = serde_json::to_string(&fingerprint).unwrap();
        assert_eq!(json, format!("\"{}\"", "07".repeat(32)));
        assert_eq!(serde_json::from_str::<Fingerprint>(&json).unwrap(), fingerprint);
        let wire = postcard::to_allocvec(&fingerprint).unwrap();
        assert_eq!(wire, vec![7; 32]);
        assert_eq!(postcard::from_bytes::<Fingerprint>(&wire).unwrap(), fingerprint);
    }
}
